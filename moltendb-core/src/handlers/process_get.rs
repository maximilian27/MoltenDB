use serde_json::{Value, json};
use crate::validation;
use crate::{engine, query};
use std::collections::{HashMap, BinaryHeap};
use std::cmp::Ordering;
use tracing::debug;



/// Compare two optional JSON values for sorting purposes.
///
/// Rules:
///   - Both numbers  → numeric (f64) comparison.
///   - Both strings  → lexicographic comparison.
///   - One or both missing/null → the missing value sorts to the end
///     (treated as greater than any real value so nulls appear last).
///   - Mixed types   → fall back to string representation comparison.
fn compare_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    match (a, b) {
        // Both missing → equal.
        (None, None) => std::cmp::Ordering::Equal,
        // Missing sorts to the end (greater than anything).
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(va), Some(vb)) => {
            // Try numeric comparison first.
            if let (Some(na), Some(nb)) = (va.as_f64(), vb.as_f64()) {
                return na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal);
            }
            // Try string comparison.
            if let (Some(sa), Some(sb)) = (va.as_str(), vb.as_str()) {
                return sa.cmp(sb);
            }
            // Fall back to comparing JSON string representations.
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
    // Validate the request structure before doing any work.
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    // Reject any unknown top-level properties so typos are caught immediately
    // (e.g. "filed" instead of "fields" would otherwise be silently ignored).
    const GET_ALLOWED: &[&str] = &[
        "collection", "keys", "where", "fields", "excludedFields",
        "joins", "sort", "count", "offset",
        "_allowed_prefixes", // internal: injected server-side by handle_get for prefix-scoped tokens
    ];
    if let Err(e) = validation::validate_allowed_properties(payload, GET_ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    // Extract the collection name — default to "default" if missing.
    let col_name = payload["collection"].as_str().unwrap_or("default");
    // The WHERE clause is optional — None means "return all documents".
    let where_clause = payload.get("where");

    // ── Index-accelerated query planning ──────────────────────────────────────
    // Before doing a full collection scan, check if any WHERE field has an index.
    // If so, use the index to get a small candidate set instead of scanning all docs.
    let mut candidate_keys: Vec<String> = Vec::new();
    let mut used_index = false;

    if let Some(query_obj) = where_clause.and_then(|w| w.as_object()) {
        for (field, condition) in query_obj {
            // Track this query for auto-indexing (increments the heatmap counter).
            db.track_query(col_name, field);
            // The index key format is "collection:field" (e.g. "users:role").
            let index_key = format!("{}:{}", col_name, field);

            // Check if an index exists for this field.
            if let Some(field_index) = db.indexes.get(&index_key) {

                // ── Exact equality index lookup (O(1)) ────────────────────────
                // If the condition is a plain value or a $eq/$equals operator,
                // we can look up the matching keys directly in the index.
                let target_val = if condition.is_object() {
                    // Operator object — look for $eq or $equals.
                    condition.get("$eq").or(condition.get("$equals"))
                } else {
                    // Plain value — implicit equality.
                    Some(condition)
                };

                if let Some(val) = target_val {
                    let val_str = val.to_string();
                    if let Some(key_set) = field_index.get(&val_str) {
                        // Found matching keys in the index — use them as candidates.
                        candidate_keys = key_set.iter().map(|k| k.clone()).collect();
                        used_index = true;
                        debug!("⚡ Optimizer: Using index for {}", index_key);
                        break; // One index lookup is enough
                    }
                }

                // ── Range query index scan ────────────────────────────────────
                // For $gt/$gte/$lt/$lte, we can't do a single hash lookup, but we
                // can scan the index values (which are much fewer than all documents)
                // to find matching keys. This is faster than a full collection scan.
                if !used_index
                    && let Some(cond_obj) = condition.as_object() {
                        // Check if any range operator is present in the condition.
                        let has_range = cond_obj.keys().any(|op| {
                            matches!(op.as_str(), "$gt" | "$greaterThan" | "$gte" | "$lt" | "$lessThan" | "$lte")
                        });

                        if has_range {
                            // Collect all document keys whose indexed field value
                            // satisfies the range condition.
                            let mut matched_keys: std::collections::HashSet<String> = std::collections::HashSet::new();

                            for entry in field_index.iter() {
                                let index_val_str = entry.key();
                                // Parse the stored index value as a number for comparison.
                                // Index values are stored as strings (e.g. "10"), so we
                                // strip quotes and parse as f64.
                                if let Ok(index_num) = index_val_str.trim_matches('"').parse::<f64>() {
                                    // Check all range operators against this index value.
                                    let passes = cond_obj.iter().all(|(op, op_val)| {
                                        if let Some(op_num) = op_val.as_f64() {
                                            match op.as_str() {
                                                "$gt" | "$greaterThan" => index_num > op_num,
                                                "$gte"                 => index_num >= op_num,
                                                "$lt" | "$lessThan"    => index_num < op_num,
                                                "$lte"                 => index_num <= op_num,
                                                _ => true, // Non-range operators pass through
                                            }
                                        } else {
                                            true // Non-numeric operator value — skip
                                        }
                                    });
                                    if passes {
                                        // This index value is in range — collect its document keys.
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

    // ── Read post-fetch query parameters early ───────────────────────────────
    // We need sort/count/offset before fetching so we can pick the right path:
    // for a sort+count query without joins or keys we stream documents directly
    // into a bounded heap (O(cap) extra memory) instead of materialising the
    // full collection in a HashMap first (which would double peak memory on
    // large collections).
    let fields_req = payload.get("fields").and_then(|f| f.as_array());
    let excluded_fields_req = payload.get("excludedFields").and_then(|f| f.as_array());

    if fields_req.is_some() && excluded_fields_req.is_some() {
        return (400, json!({ "error": "'fields' and 'excludedFields' cannot be used together — use one or the other", "statusCode": 400 }));
    }

    let joins_req = payload.get("joins").and_then(|j| j.as_array());
    let count_limit: Option<usize> = payload.get("count").and_then(|c| c.as_u64()).map(|n| n as usize);
    let offset: usize = payload.get("offset").and_then(|c| c.as_u64()).map(|n| n as usize).unwrap_or(0);
    let sort_specs = payload.get("sort").and_then(|s| s.as_array()).cloned();

    // Helper: build a sort comparator from sort specs. Used both by the
    // streaming `scan_top_n` path and by the post-processing sort step below.
    // Returns `Less` for the "better" item (so a max-heap evicts the worst).
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
    let has_keys = matches!(payload.get("keys"), Some(Value::String(_)) | Some(Value::Array(_)));

    // Track whether the streaming top-N path was taken — if so the result is
    // already sorted best-first and capped at (offset + count), so the later
    // sort/heap step is skipped and only the offset+count slicing remains.
    let mut presorted_capped: Option<Vec<(String, Value)>> = None;

    // ── Fetch documents ───────────────────────────────────────────────────────
    // If we found candidate keys via an index, fetch only those documents.
    // Otherwise, fall back to the keys specified in the request, or get all.
    let results: HashMap<String, Value> = if used_index {
        // Index hit — fetch only the candidate documents (fast path).
        db.get(col_name, candidate_keys)
    } else {
        match payload.get("keys") {
            // Single key lookup: { "keys": "u1" }
            Some(Value::String(k)) => db.get(col_name, vec![k.to_string()]),
            // Batch key lookup: { "keys": ["u1", "u2"] }
            Some(Value::Array(arr)) => {
                let ks = arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                db.get(col_name, ks)
            }
            // No keys specified.
            _ => {
                // ── Streaming top-N (sort + count, no joins) ──────────────────
                // The big win: documents flow from DashMap → bounded heap of
                // size (offset+count) → result. No intermediate HashMap of all
                // matching docs. Peak memory: O(cap) instead of O(matches).
                if !joins_present
                    && let (Some(specs), Some(limit)) = (sort_specs.as_ref(), count_limit) {
                        let cap = offset + limit;
                        let cmp = make_cmp(specs);
                        let where_clone = where_clause.cloned();
                        let predicate = move |doc: &Value| -> bool {
                            match &where_clone {
                                Some(clause) => query::evaluate_where(doc, clause).unwrap_or(false),
                                None => true,
                            }
                        };
                        presorted_capped = Some(db.scan_top_n(col_name, predicate, cmp, cap));
                        // We still feed the (already-sorted, capped) results
                        // through the join/projection loop below. Use a tiny
                        // HashMap built only from what survived the heap.
                        presorted_capped.as_ref()
                            .map(|v| v.iter().cloned().collect::<HashMap<_, _>>())
                            .unwrap_or_default()
                    } else if let Some(clause) = where_clause
                        && !joins_present {
                            // ── Lazy WHERE scan (no sort+count) ──────────────
                            // Filter while iterating — only matching docs are
                            // cloned. Saves O(n) clone vs. get_all.
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

    // Suppress the unused-variable warning when neither the streaming path
    // nor the keys path applies.
    let _ = has_keys;

    let mut final_results = HashMap::new();

    for (key, mut doc) in results {
        // ── Cross-collection joins ────────────────────────────────────────────
        // For each join spec, look up the related document and embed it.
        let mut join_aliases = Vec::new();
        if let Some(joins) = joins_req {
            for join_spec in joins {
                // ── Join syntax ───────────────────────────────────────────────
                //
                // Required syntax:
                //   { "order_details": { "from": "orders", "on": "active_order", "fields": [...] } }
                //   → alias = "order_details", target_col = "orders", fk_field = "active_order"
                //
                // The join spec object has exactly one key — the alias — whose value is
                // an object containing "from" (target collection) and "on" (foreign key
                // field path, dot-notation supported). "fields" is optional projection.
                // Join specs that don't match this shape are silently skipped.
                let (target_col, fk_field, alias, join_fields): (String, String, String, Option<&Vec<serde_json::Value>>) = {
                    // Try new syntax: find a key whose value is an object with a "from" field.
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
                        // Join spec does not use the required syntax — skip this join.
                        continue;
                    }
                };
                let target_col = target_col.as_str();
                let fk_field   = fk_field.as_str();
                join_aliases.push(alias.clone());

                // Read the foreign key value from the current document.
                let fk_val_opt = {
                    let mut current: &Value = &doc;
                    for part in fk_field.split('.') {
                        if let Some(v) = current.get(part) { current = v; }
                        else { current = &Value::Null; break; }
                    }
                    current.as_str().map(|s| s.to_string())
                };

                // If the foreign key exists, look up the related document.
                if let Some(fk_val) = fk_val_opt
                    && let Some(related_doc) = db.get(target_col, vec![fk_val.clone()]).remove(&fk_val) {
                        // Optionally project the joined document to specific fields.
                        let final_related = if let Some(j_fields) = join_fields {
                            query::project(&related_doc, j_fields)
                        } else {
                            related_doc
                        };
                        // Embed the joined document under the alias key.
                        if let Some(doc_obj) = doc.as_object_mut() {
                            doc_obj.insert(alias.clone(), final_related);
                        }
                    }
            }
        }

        // ── Prefix Gatekeeper ─────────────────────────────────────────────────
        // Fast key.starts_with() check injected by handle_get for prefix-scoped tokens
        // (e.g. "read:laptops:store_A_*"). Runs before the expensive AST evaluator so
        // unauthorised keys are skipped without ever entering evaluate_where.
        if let Some(prefixes) = payload.get("_allowed_prefixes").and_then(|p| p.as_array())
            && !prefixes.is_empty() {
                let allowed = prefixes.iter()
                    .filter_map(|p| p.as_str())
                    .any(|prefix| key.starts_with(prefix));
                if !allowed { continue; }
            }

        // ── WHERE filtering ───────────────────────────────────────────────────
        // Even if we used an index, we still apply the full WHERE clause here.
        // The index narrows the candidate set but doesn't guarantee all conditions
        // are met (e.g. a compound WHERE with multiple fields).
        if let Some(clause) = where_clause {
            let matches = match query::evaluate_where(&doc, clause) {
                Ok(m) => m,
                Err(e) => return (400, json!({ "error": e.to_string(), "statusCode": 400 })),
            };
            if !matches { continue; }
        }

        // ── Field projection / exclusion ──────────────────────────────────────
        let mut processed_doc = if let Some(fields) = fields_req {
            // Project: keep only the requested fields.
            let mut projected = query::project(&doc, fields);

            // Also include any joined fields that were embedded above.
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
            // Exclude: remove the specified fields, keep everything else.
            query::exclude(&doc, excluded)
        } else {
            // No projection — return the full document.
            doc.clone()
        };

        // ALWAYS include _v, regardless of projection or exclusion.
        // It's essential for concurrency control.
        if let Some(v_val) = doc.get("_v")
            && let Some(obj) = processed_doc.as_object_mut() {
                obj.insert("_v".to_string(), v_val.clone());
            }

        final_results.insert(key, processed_doc);
    }

    // Return null if no documents matched.
    if final_results.is_empty() { return (404, json!({ "error": "No documents found", "statusCode": 404 })); }

    // For single-key lookups ({ "keys": "u1" }), return just the document value
    // directly — no array wrapper, no _key injection. The caller already knows
    // which key they asked for.
    if let Some(Value::String(_)) = payload.get("keys")
        && let Some(first_val) = final_results.values().next().cloned() {
            return (200, first_val);
        }

    // ── Apply sort + pagination ───────────────────────────────────────────────
    // If the streaming top-N path was used, the result is already sorted
    // best-first and capped at (offset + count) — we just need to replace
    // each entry's value with the (possibly projected/joined) version from
    // `final_results`, then apply offset slicing.
    let entries: Vec<(String, Value)> = if let Some(presorted) = presorted_capped {
        presorted.into_iter()
            .filter_map(|(k, _v)| final_results.remove(&k).map(|nv| (k, nv)))
            .collect()
    } else { match (sort_specs, count_limit) {
        // ── Bounded top-N heap (sort + count) ────────────────────────────────
        // Keeps only (offset + count) items in the heap at any time.
        // This avoids materialising all matching documents for sorting when
        // only a small slice of the result set is needed.
        (Some(specs), Some(limit)) => {
            let cap = offset + limit;
            let cmp = make_cmp(&specs);

            // Wrapper that reverses the comparison so BinaryHeap (a max-heap)
            // evicts the *worst* candidate when the heap is full — leaving the
            // best `cap` items inside.
            struct HeapItem(String, Value, std::sync::Arc<dyn Fn(&Value, &Value) -> Ordering + Send + Sync>);
            impl PartialEq for HeapItem { fn eq(&self, o: &Self) -> bool { (self.2)(&self.1, &o.1) == Ordering::Equal } }
            impl Eq for HeapItem {}
            impl PartialOrd for HeapItem {
                fn partial_cmp(&self, o: &Self) -> Option<Ordering> { Some(self.cmp(o)) }
            }
            impl Ord for HeapItem {
                // Max-heap: we want the *worst* candidate (per the sort comparator)
                // to bubble to the top so it gets evicted first when we push beyond
                // capacity. The user comparator returns `Less` for "better" items
                // (e.g. higher price under desc), so the worst item has the greatest
                // ordering — exactly what we want for a max-heap top.
                fn cmp(&self, o: &Self) -> Ordering { (self.2)(&self.1, &o.1) }
            }

            let cmp_arc: std::sync::Arc<dyn Fn(&Value, &Value) -> Ordering + Send + Sync> =
                std::sync::Arc::new(cmp);

            let mut heap: BinaryHeap<HeapItem> = BinaryHeap::with_capacity(cap + 1);
            for (k, v) in final_results {
                heap.push(HeapItem(k, v, cmp_arc.clone()));
                if heap.len() > cap {
                    heap.pop(); // evict the worst item
                }
            }

            // Drain the heap — items come out in worst-first order, so reverse.
            let mut sorted: Vec<(String, Value)> = heap.into_sorted_vec()
                .into_iter()
                .map(|item| (item.0, item.1))
                .collect();
            // into_sorted_vec gives ascending order (best first for our reversed heap),
            // which is exactly the order we want.
            sorted.truncate(cap);
            sorted
        }

        // ── Full sort, no count limit ─────────────────────────────────────────
        (Some(specs), None) => {
            let cmp = make_cmp(&specs);
            let mut v: Vec<(String, Value)> = final_results.into_iter().collect();
            v.sort_by(|(_, a), (_, b)| cmp(a, b));
            v
        }

        // ── No sort — collect in arbitrary order ──────────────────────────────
        _ => final_results.into_iter().collect(),
    } };

    // ── Apply offset and count (pagination) ───────────────────────────────────
    let iter = entries.into_iter().skip(offset);
    let mut iter: Box<dyn Iterator<Item = (String, Value)>> = Box::new(iter);
    if let Some(limit) = count_limit {
        iter = Box::new(iter.take(limit));
    }

    let array: Vec<Value> = iter.map(|(k, mut doc)| {
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("_key".to_string(), Value::String(k));
        }
        doc
    }).collect();

    (200, Value::Array(array))
}

