use serde_json::{Value, json};
use crate::validation;
use crate::{engine, query};
use std::collections::HashMap;
use std::cmp::Ordering;
use tracing::debug;

/// Compare two optional JSON values for sorting purposes.
/// - Both numbers  → numeric (f64) comparison.
/// - Both strings  → lexicographic comparison.
/// - Missing/null  → sorts to the end.
/// - Mixed types   → fall back to string representation.
fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(va), Some(vb)) => {
            if let (Some(na), Some(nb)) = (va.as_f64(), vb.as_f64()) {
                return na.partial_cmp(&nb).unwrap_or(Ordering::Equal);
            }
            if let (Some(sa), Some(sb)) = (va.as_str(), vb.as_str()) {
                return sa.cmp(sb);
            }
            va.to_string().cmp(&vb.to_string())
        }
    }
}

/// Handle a GET (query) request.
///
/// Supports:
///   - Single key lookup:  { "collection": "users", "keys": "u1" }
///   - Batch key lookup:   { "collection": "users", "keys": ["u1", "u2"] }
///   - Full collection:    { "collection": "users" }
///   - WHERE filtering:    { "collection": "users", "where": { "role": "admin" } }
///   - Field projection:   { "collection": "users", "fields": ["name", "age"] }
///   - Field exclusion:    { "collection": "users", "excludedFields": ["role"] }
///   - Cross-collection joins: { "joins": [{ "order_details": { "from": "orders", "on": "active_order", "fields": [...] } }] }
///   - Pagination:         { "count": 10, "offset": 0 }
///   - Sorting:            { "sort": ["age"] }  or  { "sort": [{ "field": "age", "order": "desc" }] }
pub fn process_get(db: &engine::Db, payload: &Value, max_body_size: usize, max_keys_per_request: usize) -> (u16, Value) {
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    const GET_ALLOWED: &[&str] = &[
        "collection", "keys", "where", "fields", "excludedFields",
        "joins", "sort", "count", "offset",
        "_allowed_prefixes",
    ];
    if let Err(e) = validation::validate_allowed_properties(payload, GET_ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    let col_name = payload["collection"].as_str().unwrap_or("default");
    let where_clause = payload.get("where");

    // ── Index-accelerated query planning ──────────────────────────────────────
    let mut candidate_keys: Vec<String> = Vec::new();
    let mut used_index = false;

    if let Some(query_obj) = where_clause.and_then(|w| w.as_object()) {
        for (field, condition) in query_obj {
            db.track_query(col_name, field);
            let index_key = format!("{}:{}", col_name, field);

            if let Some(field_index) = db.indexes.get(&index_key) {
                // Exact equality lookup.
                let target_val = if condition.is_object() {
                    condition.get("$eq").or(condition.get("$equals"))
                } else {
                    Some(condition)
                };

                if let Some(val) = target_val {
                    let val_str = val.to_string();
                    if let Some(key_set) = field_index.get(&val_str) {
                        candidate_keys = key_set.iter().map(|k| k.clone()).collect();
                        used_index = true;
                        debug!("⚡ Optimizer: Using index for {}", index_key);
                        break;
                    }
                }

                // Range query index scan.
                if !used_index
                    && let Some(cond_obj) = condition.as_object() {
                        let has_range = cond_obj.keys().any(|op| {
                            matches!(op.as_str(), "$gt" | "$greaterThan" | "$gte" | "$lt" | "$lessThan" | "$lte")
                        });

                        if has_range {
                            let mut matched_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
                            for entry in field_index.iter() {
                                let index_val_str = entry.key();
                                if let Ok(index_num) = index_val_str.trim_matches('"').parse::<f64>() {
                                    let passes = cond_obj.iter().all(|(op, op_val)| {
                                        if let Some(op_num) = op_val.as_f64() {
                                            match op.as_str() {
                                                "$gt" | "$greaterThan" => index_num > op_num,
                                                "$gte"                 => index_num >= op_num,
                                                "$lt" | "$lessThan"    => index_num < op_num,
                                                "$lte"                 => index_num <= op_num,
                                                _ => true,
                                            }
                                        } else {
                                            true
                                        }
                                    });
                                    if passes {
                                        for k in entry.value().iter() {
                                            matched_keys.insert(k.clone());
                                        }
                                    }
                                }
                            }
                            if !matched_keys.is_empty() {
                                candidate_keys = matched_keys.into_iter().collect();
                                used_index = true;
                                debug!("⚡ Optimizer: Using index for range query on {}", index_key);
                                break;
                            }
                        }
                    }
            }
        }
    }

    let fields_req = payload.get("fields").and_then(|f| f.as_array());
    let excluded_fields_req = payload.get("excludedFields").and_then(|f| f.as_array());

    if fields_req.is_some() && excluded_fields_req.is_some() {
        return (400, json!({ "error": "'fields' and 'excludedFields' cannot be used together — use one or the other", "statusCode": 400 }));
    }

    let joins_req = payload.get("joins").and_then(|j| j.as_array());
    let count_limit: Option<usize> = payload.get("count").and_then(|c| c.as_u64()).map(|n| n as usize);
    let offset: usize = payload.get("offset").and_then(|c| c.as_u64()).map(|n| n as usize).unwrap_or(0);
    let sort_specs = payload.get("sort").and_then(|s| s.as_array()).cloned();

    // Build a sort comparator from sort specs.
    let make_cmp = |specs: &Vec<Value>| {
        let specs = specs.clone();
        move |doc_a: &Value, doc_b: &Value| -> Ordering {
            for spec in &specs {
                let (field, descending) = if let Some(field_str) = spec.as_str() {
                    (field_str.to_string(), false)
                } else if let Some(obj) = spec.as_object() {
                    let field = obj.get("field").and_then(|f| f.as_str()).unwrap_or("").to_string();
                    let desc  = obj.get("order").and_then(|o| o.as_str())
                        .map(|o| o.eq_ignore_ascii_case("desc")).unwrap_or(false);
                    (field, desc)
                } else {
                    continue;
                };
                if field.is_empty() { continue; }
                let parts: Vec<&str> = field.split('.').collect();
                let val_a = query::get_nested_value(doc_a, &parts);
                let val_b = query::get_nested_value(doc_b, &parts);
                let ord = compare_values(val_a.as_ref(), val_b.as_ref());
                if ord != Ordering::Equal {
                    return if descending { ord.reverse() } else { ord };
                }
            }
            Ordering::Equal
        }
    };

    let joins_present = joins_req.map(|a| !a.is_empty()).unwrap_or(false);
    let has_prefix_filter = payload.get("_allowed_prefixes")
        .and_then(|p| p.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    // ── Fast path: sort + count with no keys/joins/prefix filter ─────────────
    // Use scan_top_n to keep only offset+count items in a bounded heap,
    // avoiding materialising the entire collection into RAM.
    let no_keys = !matches!(payload.get("keys"), Some(Value::String(_)) | Some(Value::Array(_)));
    if no_keys && !used_index && !joins_present && !has_prefix_filter
        && let Some(ref specs) = sort_specs
        && let Some(limit) = count_limit
    {
        let cap = offset + limit;
        let cmp_specs = specs.clone();
        let cmp = make_cmp(&cmp_specs);
        let predicate_clause = where_clause.cloned();
        let predicate = move |doc: &Value| -> bool {
            match &predicate_clause {
                Some(clause) => query::evaluate_where(doc, clause).unwrap_or(false),
                None => true,
            }
        };
        let top_items = db.scan_top_n(col_name, predicate, cmp, cap);

        // Apply projection/exclusion and build the response array.
        let array: Vec<Value> = top_items.into_iter().skip(offset).map(|(k, mut doc)| {
            let mut processed = if let Some(fields) = fields_req {
                let mut projected = query::project(&doc, fields);
                if let Some(v_val) = doc.get("_v")
                    && let Some(obj) = projected.as_object_mut() {
                        obj.insert("_v".to_string(), v_val.clone());
                    }
                projected
            } else if let Some(excluded) = excluded_fields_req {
                query::exclude(&doc, excluded)
            } else {
                let v_val = doc.get("_v").cloned();
                if let Some(v_val) = v_val
                    && let Some(obj) = doc.as_object_mut() {
                        obj.insert("_v".to_string(), v_val);
                    }
                doc
            };
            if let Some(obj) = processed.as_object_mut() {
                obj.insert("_key".to_string(), Value::String(k));
            }
            processed
        }).collect();

        if array.is_empty() {
            return (404, json!({ "error": "No documents found", "statusCode": 404 }));
        }
        return (200, Value::Array(array));
    }

    // ── Fetch documents ───────────────────────────────────────────────────────
    let results: HashMap<String, Value> = if used_index {
        db.get(col_name, candidate_keys)
    } else {
        match payload.get("keys") {
            Some(Value::String(k)) => db.get(col_name, vec![k.to_string()]),
            Some(Value::Array(arr)) => {
                let ks = arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                db.get(col_name, ks)
            }
            _ => {
                if let Some(clause) = where_clause
                    && !joins_present {
                        let clause = clause.clone();
                        db.get_filtered(
                            col_name,
                            move |doc| query::evaluate_where(doc, &clause).unwrap_or(false),
                            0,
                            None,
                        )
                    } else {
                        db.get_all(col_name)
                    }
            }
        }
    };

    let mut final_results = HashMap::new();

    for (key, mut doc) in results {
        // ── Cross-collection joins ────────────────────────────────────────────
        let mut join_aliases = Vec::new();
        if let Some(joins) = joins_req {
            for join_spec in joins {
                let (target_col, fk_field, alias, join_fields): (String, String, String, Option<&Vec<serde_json::Value>>) = {
                    let new_syntax = join_spec.as_object().and_then(|obj| {
                        obj.iter().find_map(|(k, v)| {
                            if let Some(inner) = v.as_object() {
                                if inner.contains_key("from") {
                                    let from = inner.get("from").and_then(|f| f.as_str()).unwrap_or("").to_string();
                                    let on   = inner.get("on").and_then(|f| f.as_str()).unwrap_or("").to_string();
                                    Some((from, on, k.clone(), inner.get("fields").and_then(|f| f.as_array())))
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                    });

                    if let Some((from, on, al, fields)) = new_syntax {
                        (from, on, al, fields)
                    } else {
                        continue;
                    }
                };
                let target_col = target_col.as_str();
                let fk_field   = fk_field.as_str();
                join_aliases.push(alias.clone());

                let fk_val_opt = {
                    let mut current: &Value = &doc;
                    for part in fk_field.split('.') {
                        if let Some(v) = current.get(part) { current = v; }
                        else { current = &Value::Null; break; }
                    }
                    current.as_str().map(|s| s.to_string())
                };

                if let Some(fk_val) = fk_val_opt
                    && let Some(related_doc) = db.get(target_col, vec![fk_val.clone()]).remove(&fk_val) {
                        let final_related = if let Some(j_fields) = join_fields {
                            query::project(&related_doc, j_fields)
                        } else {
                            related_doc
                        };
                        if let Some(doc_obj) = doc.as_object_mut() {
                            doc_obj.insert(alias.clone(), final_related);
                        }
                    }
            }
        }

        // ── Prefix gatekeeper ─────────────────────────────────────────────────
        if let Some(prefixes) = payload.get("_allowed_prefixes").and_then(|p| p.as_array())
            && !prefixes.is_empty() {
                let allowed = prefixes.iter()
                    .filter_map(|p| p.as_str())
                    .any(|prefix| key.starts_with(prefix));
                if !allowed { continue; }
            }

        // ── WHERE filtering ───────────────────────────────────────────────────
        if let Some(clause) = where_clause {
            let matches = match query::evaluate_where(&doc, clause) {
                Ok(m) => m,
                Err(e) => return (400, json!({ "error": e.to_string(), "statusCode": 400 })),
            };
            if !matches { continue; }
        }

        // ── Field projection / exclusion ──────────────────────────────────────
        let mut processed_doc = if let Some(fields) = fields_req {
            let mut projected = query::project(&doc, fields);
            if !join_aliases.is_empty()
                && let Some(doc_obj) = doc.as_object()
                    && let Some(proj_obj) = projected.as_object_mut() {
                        for alias in &join_aliases {
                            if let Some(joined_val) = doc_obj.get(alias) {
                                proj_obj.insert(alias.clone(), joined_val.clone());
                            }
                        }
                    }
            projected
        } else if let Some(excluded) = excluded_fields_req {
            query::exclude(&doc, excluded)
        } else {
            doc.clone()
        };

        // Always include _v for concurrency control.
        if let Some(v_val) = doc.get("_v")
            && let Some(obj) = processed_doc.as_object_mut() {
                obj.insert("_v".to_string(), v_val.clone());
            }

        final_results.insert(key, processed_doc);
    }

    if final_results.is_empty() { return (404, json!({ "error": "No documents found", "statusCode": 404 })); }

    // Single-key lookup — return the document directly, no array wrapper.
    if let Some(Value::String(_)) = payload.get("keys")
        && let Some(first_val) = final_results.values().next().cloned() {
            return (200, first_val);
        }

    // ── Sort + pagination ─────────────────────────────────────────────────────
    let mut entries: Vec<(String, Value)> = final_results.into_iter().collect();

    if let Some(specs) = sort_specs {
        let cmp = make_cmp(&specs);
        entries.sort_by(|(_, a), (_, b)| cmp(a, b));
    }

    if let Some(limit) = count_limit {
        entries.truncate(offset + limit);
    }

    let array: Vec<Value> = entries.into_iter().skip(offset).map(|(k, mut doc)| {
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("_key".to_string(), Value::String(k));
        }
        doc
    }).collect();

    (200, Value::Array(array))
}
