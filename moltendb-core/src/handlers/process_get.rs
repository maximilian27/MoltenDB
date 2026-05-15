use serde_json::{Value, json};
use crate::validation;
use crate::{engine, query};
use crate::engine::ttl;
use super::get::{shape_doc, make_comparator, fetch_documents, apply_joins};

/// Handle a GET (query) request.
///
/// Supported parameters:
///   - `collection`       -- target collection (default: "default")
///   - `keys`             -- single key (string) or batch (string[])
///   - `where`            -- filter object; operators: $eq $ne $gt $gte $lt $lte $contains $in $nin $or $and
///   - `fields`           -- GraphQL-style inclusion list (dot-notation supported)
///   - `excludedFields`   -- exclusion list (mutually exclusive with `fields`)
///   - `excluded_fields`  -- snake_case alias for `excludedFields`
///   - `joins`            -- cross-collection joins: [{ "<alias>": { "from": "<col>", "on": "<fk_field>", "fields": [...] } }]
///   - `sort`             -- sort specs: [{ "field": "price", "order": "asc"|"desc" }]
///   - `count`            -- max results after sort (pagination limit; default 100, max 1000)
///   - `offset`           -- results to skip after sort (pagination offset)
///   - `_allowed_prefixes`-- internal: restrict results to keys with these prefixes
pub fn process_get(db: &engine::Db, payload: &Value, max_body_size: usize, max_keys_per_request: usize) -> (u16, Value) {
    // -- Validation -------------------------------------------------------
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    const ALLOWED: &[&str] = &[
        "collection", "keys", "where", "fields", "excludedFields", "excluded_fields",
        "joins", "sort", "count", "offset", "_allowed_prefixes",
    ];
    if let Err(e) = validation::validate_allowed_properties(payload, ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    let fields_req   = payload.get("fields").and_then(|f| f.as_array());
    let excluded_req = payload.get("excludedFields")
        .or_else(|| payload.get("excluded_fields"))
        .and_then(|f| f.as_array());
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
    // Capture current time once for TTL expiry checks.
    let query_time = ttl::now_ms();
    // Check collection-level TTL expiry -- O(1), not per-document.
    if let Some(exp) = db.get_ttl_expiry(col_name) {
        if ttl::collection_is_expired(exp, query_time) {
            return (404, json!({ "error": "No documents found", "statusCode": 404 }));
        }
    }
    // Compute the virtual _expiresAt value once for the whole request.
    let expires_val: Option<Value> = db.get_ttl_expiry(col_name)
        .map(|ms| Value::String(ttl::ms_to_iso(ms)));
    let allowed_prefixes = payload.get("_allowed_prefixes").and_then(|p| p.as_array());

    // -- Fast path: sort + count with no joins / keys / prefix filter ------
    // Uses a bounded heap of capacity (offset + count) so peak RAM is O(offset+count).
    let no_keys   = payload.get("keys").is_none();
    let no_joins  = joins_req.map(|j| j.is_empty()).unwrap_or(true);
    let no_prefix = allowed_prefixes.map(|p| p.is_empty()).unwrap_or(true);

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
                .filter_map(|(k, doc)| shape_doc(doc, &k, fields_req, excluded_req, &[], expires_val.clone()))
                .collect();
            if array.is_empty() {
                return (404, json!({ "error": "No documents found", "statusCode": 404 }));
            }
            return (200, Value::Array(array));
        }
    }

    // -- Fetch documents --------------------------------------------------
    let has_joins = joins_req.map(|j| !j.is_empty()).unwrap_or(false);
    let raw = fetch_documents(db, col_name, payload, where_clause, has_joins, offset, count_limit);

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

        // Cross-collection joins.
        let join_aliases = if let Some(joins) = joins_req {
            apply_joins(db, &mut doc, joins)
        } else {
            vec![]
        };

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
        match shape_doc(doc, &key, fields_req, excluded_req, &aliases, expires_val.clone()) {
            None => return (404, json!({ "error": "No documents found", "statusCode": 404 })),
            Some(mut out) => {
                if let Some(obj) = out.as_object_mut() { let _ = obj.remove("_key"); }
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
        .filter_map(|(k, doc, aliases)| shape_doc(doc, &k, fields_req, excluded_req, &aliases, expires_val.clone()))
        .collect();

    if array.is_empty() {
        return (404, json!({ "error": "No documents found", "statusCode": 404 }));
    }

    (200, Value::Array(array))
}
