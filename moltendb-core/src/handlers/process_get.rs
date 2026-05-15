use serde_json::{Value, json};
use crate::validation;
use crate::{engine, query};
use crate::engine::ttl;
use std::collections::HashMap;
use std::cmp::Ordering;

/// Compare two optional JSON values for sorting.
/// Numbers -> numeric, strings -> lexicographic, missing/null -> sorts last.
fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None)       => Ordering::Equal,
        (None, Some(_))    => Ordering::Greater,
        (Some(_), None)    => Ordering::Less,
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

/// Build a sort comparator from a `sort` spec array.
/// Each spec is either a plain string field name or `{ "field": "...", "order": "asc"|"desc" }`.
fn make_comparator(specs: Vec<Value>) -> impl Fn(&Value, &Value) -> Ordering {
    move |a: &Value, b: &Value| {
        for spec in &specs {
            let (field, descending) = if let Some(s) = spec.as_str() {
                (s.to_string(), false)
            } else if let Some(obj) = spec.as_object() {
                let f = obj.get("field").and_then(|f| f.as_str()).unwrap_or("").to_string();
                let d = obj.get("order").and_then(|o| o.as_str())
                    .map(|o| o.eq_ignore_ascii_case("desc"))
                    .unwrap_or(false);
                (f, d)
            } else {
                continue;
            };
            if field.is_empty() { continue; }
            let parts: Vec<&str> = field.split('.').collect();
            let ord = compare_values(
                query::get_nested_value(a, &parts).as_ref(),
                query::get_nested_value(b, &parts).as_ref(),
            );
            if ord != Ordering::Equal {
                return if descending { ord.reverse() } else { ord };
            }
        }
        Ordering::Equal
    }
}

/// Apply field projection or exclusion to a document, preserving `_v`, `_createdAt`, `_modifiedAt`, `_expiresAt` and inserting `_key`.
/// Returns `None` if the document has expired (lazy TTL eviction).
fn shape_doc(doc: Value, key: &str, fields: Option<&Vec<Value>>, excluded: Option<&Vec<Value>>, join_aliases: &[String], current_time_ms: u64) -> Option<Value> {
    // Lazy TTL eviction -- treat expired documents as not found.
    if ttl::is_expired(&doc, current_time_ms) {
        return None;
    }

    let v_val           = doc.get("_v").cloned();
    let created_val     = doc.get("_createdAt").cloned();
    let modified_val    = doc.get("_modifiedAt").cloned();
    let expires_val     = doc.get("_expiresAt").cloned();

    let mut out = if let Some(f) = fields {
        let mut projected = query::project(&doc, f);
        // Re-attach any joined sub-documents that were embedded before projection.
        if let (Some(src), Some(dst)) = (doc.as_object(), projected.as_object_mut()) {
            for alias in join_aliases {
                if let Some(v) = src.get(alias) {
                    dst.insert(alias.clone(), v.clone());
                }
            }
        }
        projected
    } else if let Some(ex) = excluded {
        query::exclude(&doc, ex)
    } else {
        doc
    };

    if let Some(obj) = out.as_object_mut() {
        if let Some(v) = v_val           { obj.insert("_v".to_string(), v); }
        if let Some(v) = created_val     { obj.insert("_createdAt".to_string(), v); }
        if let Some(v) = modified_val    { obj.insert("_modifiedAt".to_string(), v); }
        if let Some(v) = expires_val     { obj.insert("_expiresAt".to_string(), v); }
        obj.insert("_key".to_string(), Value::String(key.to_string()));
    }
    Some(out)
}

/// Handle a GET (query) request.
///
/// Supported parameters:
///   - `collection`       -- target collection (default: "default")
///   - `keys`             -- single key (string) or batch (string[])
///   - `where`            -- filter object; operators: $eq $ne $gt $gte $lt $lte $contains $in $nin $or $and
///   - `fields`           -- GraphQL-style inclusion list (dot-notation supported)
///   - `excludedFields`   -- exclusion list (mutually exclusive with `fields`)
///   - `joins`            -- cross-collection joins: [{ "<alias>": { "from": "<col>", "on": "<fk_field>", "fields": [...] } }]
///   - `sort`             -- sort specs: [{ "field": "price", "order": "asc"|"desc" }]
///   - `count`            -- max results after sort (pagination limit)
///   - `offset`           -- results to skip after sort (pagination offset)
///   - `_allowed_prefixes`-- internal: restrict results to keys with these prefixes
pub fn process_get(db: &engine::Db, payload: &Value, max_body_size: usize, max_keys_per_request: usize) -> (u16, Value) {
    // -- Validation -------------------------------------------------------
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    const ALLOWED: &[&str] = &[
        "collection", "keys", "where", "fields", "excludedFields",
        "joins", "sort", "count", "offset", "_allowed_prefixes",
    ];
    if let Err(e) = validation::validate_allowed_properties(payload, ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    let fields_req   = payload.get("fields").and_then(|f| f.as_array());
    let excluded_req = payload.get("excludedFields").and_then(|f| f.as_array());
    if fields_req.is_some() && excluded_req.is_some() {
        return (400, json!({ "error": "'fields' and 'excludedFields' cannot be used together", "statusCode": 400 }));
    }

    // -- Parse query parameters -------------------------------------------
    let col_name     = payload["collection"].as_str().unwrap_or("default");
    let where_clause = payload.get("where");
    let joins_req    = payload.get("joins").and_then(|j| j.as_array());
    let sort_specs   = payload.get("sort").and_then(|s| s.as_array()).cloned();
    const DEFAULT_COUNT: usize = 100;
    const MAX_COUNT: usize = 1_000;
    if let Some(n) = payload.get("count").and_then(|c| c.as_u64()) {
        if n as usize > MAX_COUNT {
            return (400, json!({ "error": format!("'count' cannot exceed {MAX_COUNT}"), "statusCode": 400 }));
        }
    }
    let count_limit: usize = payload.get("count")
        .and_then(|c| c.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_COUNT);
    let offset: usize = payload.get("offset").and_then(|c| c.as_u64()).map(|n| n as usize).unwrap_or(0);
    // Capture current time once for TTL expiry checks -- avoids syscall-per-doc in parallel scans.
    let query_time = ttl::now_ms();
    let allowed_prefixes = payload.get("_allowed_prefixes").and_then(|p| p.as_array());

    // -- Fast path: sort + count with no joins / keys / prefix filter ------
    // Use a bounded heap of capacity (offset + count) so peak RAM is O(offset+count)
    // rather than O(collection size).
    let no_keys    = payload.get("keys").is_none();
    let no_joins   = joins_req.map(|j| j.is_empty()).unwrap_or(true);
    let no_prefix  = allowed_prefixes.map(|p| p.is_empty()).unwrap_or(true);

    if no_keys && no_joins && no_prefix {
        if let Some(specs) = sort_specs.clone() {
            let cap = offset + count_limit;
            let cmp = make_comparator(specs);
            let where_clause_owned = where_clause.cloned();
            let top = db.scan_top_n(
                col_name,
                move |doc| where_clause_owned.as_ref()
                    .map(|c| query::evaluate_where(doc, c).unwrap_or(false))
                    .unwrap_or(true),
                cmp,
                cap,
            );
            let array: Vec<Value> = top.into_iter().skip(offset)
                .filter_map(|(k, doc)| shape_doc(doc, &k, fields_req, excluded_req, &[], query_time))
                .collect();
            if array.is_empty() {
                return (404, json!({ "error": "No documents found", "statusCode": 404 }));
            }
            return (200, Value::Array(array));
        }
    }

    // -- Fetch documents --------------------------------------------------
    let raw: HashMap<String, Value> = match payload.get("keys") {
        Some(Value::String(k)) => db.get(col_name, vec![k.clone()]),
        Some(Value::Array(arr)) => {
            let ks = arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
            db.get(col_name, ks)
        }
        _ => {
            // Full scan -- apply WHERE early when there are no joins (avoids materialising filtered-out docs).
            if let Some(clause) = where_clause
                && joins_req.map(|j| j.is_empty()).unwrap_or(true) {
                    let clause = clause.clone();
                    db.get_filtered(col_name, move |doc| query::evaluate_where(doc, &clause).unwrap_or(false), 0, None)
                } else {
                    db.get_all(col_name)
                }
        }
    };

    // -- Per-document processing ------------------------------------------
    let mut results: Vec<(String, Value, Vec<String>)> = Vec::with_capacity(raw.len());

    for (key, mut doc) in raw {
        // Prefix gatekeeper (used by scoped auth tokens).
        if let Some(prefixes) = allowed_prefixes {
            if !prefixes.is_empty() {
                let allowed = prefixes.iter().filter_map(|p| p.as_str()).any(|p| key.starts_with(p));
                if !allowed { continue; }
            }
        }

        // Cross-collection joins -- embed related document under the alias field.
        let mut join_aliases: Vec<String> = Vec::new();
        if let Some(joins) = joins_req {
            for join_spec in joins {
                let Some(obj) = join_spec.as_object() else { continue };
                let Some((alias, inner)) = obj.iter().find_map(|(k, v)| {
                    v.as_object().filter(|o| o.contains_key("from")).map(|o| (k.clone(), o))
                }) else { continue };

                let from     = inner.get("from").and_then(|f| f.as_str()).unwrap_or("");
                let on       = inner.get("on").and_then(|f| f.as_str()).unwrap_or("");
                let j_fields = inner.get("fields").and_then(|f| f.as_array());

                // Resolve the foreign key value (supports dot-notation).
                let fk_val = on.split('.').fold(Some(&doc as &Value), |cur, part| {
                    cur.and_then(|v| v.get(part))
                }).and_then(|v| v.as_str()).map(|s| s.to_string());

                if let Some(fk) = fk_val
                    && let Some(related) = db.get(from, vec![fk.clone()]).remove(&fk) {
                        let embedded = if let Some(jf) = j_fields {
                            query::project(&related, jf)
                        } else {
                            related
                        };
                        if let Some(doc_obj) = doc.as_object_mut() {
                            doc_obj.insert(alias.clone(), embedded);
                        }
                        join_aliases.push(alias);
                    }
            }
        }

        // WHERE filter (re-applied here when joins are present, since joined fields may be tested).
        if let Some(clause) = where_clause {
            match query::evaluate_where(&doc, clause) {
                Ok(true)  => {}
                Ok(false) => continue,
                Err(e)    => return (400, json!({ "error": e.to_string(), "statusCode": 400 })),
            }
        }

        results.push((key, doc, join_aliases));
    }

    if results.is_empty() {
        return (404, json!({ "error": "No documents found", "statusCode": 404 }));
    }

    // Single-key lookup -- return the document directly (no array wrapper, no _key).
    if let Some(Value::String(_)) = payload.get("keys") {
        let (key, doc, aliases) = results.remove(0);
        match shape_doc(doc, &key, fields_req, excluded_req, &aliases, query_time) {
            None => return (404, json!({ "error": "No documents found", "statusCode": 404 })),
            Some(mut out) => {
                if let Some(obj) = out.as_object_mut() { obj.remove("_key"); }
                return (200, out);
            }
        }
    }

    // -- Sort -------------------------------------------------------------
    if let Some(specs) = sort_specs {
        let cmp = make_comparator(specs);
        results.sort_by(|(_, a, _), (_, b, _)| cmp(a, b));
    }

    // -- Pagination -------------------------------------------------------
    results.truncate(offset + count_limit);

    // -- Shape and return -------------------------------------------------
    let array: Vec<Value> = results.into_iter().skip(offset)
        .filter_map(|(k, doc, aliases)| shape_doc(doc, &k, fields_req, excluded_req, &aliases, query_time))
        .collect();

    if array.is_empty() {
        return (404, json!({ "error": "No documents found", "statusCode": 404 }));
    }

    (200, Value::Array(array))
}
