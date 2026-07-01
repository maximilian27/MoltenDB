use super::get::constants::GET_ALLOWED;
use super::get::errors::GetError;
use super::get::responses::GetSuccess;
use super::get::types::{FetchParams, GetParams};
use super::get::{apply_joins, fetch_documents, make_comparator, shape_doc};
use crate::common::payload_fields::PayloadField;
use crate::engine::ttl;
use crate::handlers::common::errors::{OperationError, ValidationError};
use crate::validation;
use crate::{engine, query};
use serde_json::Value;

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
///   - `count`            -- max results after sort (pagination limit; default 100, max 1000)
///   - `offset`           -- results to skip after sort (pagination offset)
///   - `_allowed_prefixes`-- internal: restrict results to keys with these prefixes
///   - `seq`              -- single seq number (u64) or seq interval list: [{"start": u64, "end": u64}]
pub fn process_get(
    db: &engine::Db,
    payload: &Value,
    max_body_size: usize,
    max_keys_per_request: usize,
) -> (u16, Value) {
    if let Err(e) = validate_get_request(payload, max_body_size, max_keys_per_request) {
        return e;
    }

    let params = match parse_get_params(db, payload) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Fast path: sort + count with no joins / keys / prefix filter.
    // Uses a bounded heap of capacity (offset + count) so peak RAM is O(offset+count).
    let no_keys = payload.get(PayloadField::Keys.as_str()).is_none();
    let no_joins = params.joins_req.map(|j| j.is_empty()).unwrap_or(true);
    let no_prefix = params
        .allowed_prefixes
        .map(|p| p.is_empty())
        .unwrap_or(true);

    if no_keys && no_joins && no_prefix {
        if let Some(result) = run_fast_sort_path(db, payload, &params) {
            return result;
        }
    }

    // Fetch, filter, join.
    let has_joins = params.joins_req.map(|j| !j.is_empty()).unwrap_or(false);

    let default_order_asc = payload
        .get(PayloadField::Order.as_str())
        .and_then(|v| v.as_str())
        .map(|s| s == "asc")
        .unwrap_or(false);

    let fetch_params = FetchParams {
        col_name: params.col_name,
        payload,
        where_clause: params.where_clause,
        has_joins,
        has_sort: params.sort_specs.is_some(),
        has_where: params.where_clause.is_some(),
        default_order_asc,
        offset: params.offset,
        count_limit: params.count_limit,
        allowed_prefixes: params.allowed_prefixes,
    };

    let raw = fetch_documents(db, &fetch_params);

    let results = match filter_and_join_docs(db, raw, &params) {
        Ok(r) => r,
        Err(e) => return e,
    };

    if results.is_empty() {
        return GetError::NoDocumentsFound.into_response();
    }

    shape_and_return(results, payload, &params)
}

// -- Private helpers ----------------------------------------------------------

/// Validate the raw payload and check for mutually exclusive options.
/// Returns `Err((status, body))` on failure.
fn validate_get_request(
    payload: &Value,
    max_body_size: usize,
    max_keys_per_request: usize,
) -> Result<(), (u16, Value)> {
    validation::validate_request(payload, max_body_size, max_keys_per_request)
        .map_err(|e| ValidationError(e.to_string()).into_response())?;

    validation::validate_allowed_properties(payload, GET_ALLOWED)
        .map_err(|e| ValidationError(e.to_string()).into_response())?;

    let fields_req = payload
        .get(PayloadField::Fields.as_str())
        .and_then(|f| f.as_array());
    let excluded_req = payload
        .get(PayloadField::ExcludedFields.as_str())
        .and_then(|f| f.as_array());
    if fields_req.is_some() && excluded_req.is_some() {
        return Err(ValidationError(
            "'fields' and 'excludedFields' cannot be used together".to_string(),
        )
        .into_response());
    }

    let has_sort = payload.get(PayloadField::Sort.as_str()).is_some();
    let order_val = payload.get(PayloadField::Order.as_str());
    if has_sort && order_val.is_some() {
        return Err(GetError::OrderSortMutuallyExclusive.into_response());
    }
    if let Some(ord) = order_val {
        match ord.as_str() {
            Some("asc") | Some("desc") => {}
            _ => {
                return Err(GetError::InvalidOrderValue.into_response());
            }
        }
    }

    Ok(())
}

/// Parse all query parameters from the payload and perform the collection-level
/// TTL expiry check. Returns `Err((status, body))` if the collection has expired.
fn parse_get_params<'a>(
    db: &engine::Db,
    payload: &'a Value,
) -> Result<GetParams<'a>, (u16, Value)> {
    const DEFAULT_COUNT: usize = 100;
    const MAX_COUNT: usize = 1_000;

    let col_name = payload[PayloadField::Collection.as_str()]
        .as_str()
        .unwrap_or("default");
    let where_clause = payload.get(PayloadField::Where.as_str());
    let joins_req = payload
        .get(PayloadField::Joins.as_str())
        .and_then(|j| j.as_array());
    let sort_specs = payload
        .get(PayloadField::Sort.as_str())
        .and_then(|s| s.as_array())
        .cloned();
    let fields_req = payload
        .get(PayloadField::Fields.as_str())
        .and_then(|f| f.as_array());
    let excluded_req = payload
        .get(PayloadField::ExcludedFields.as_str())
        .and_then(|f| f.as_array());
    let allowed_prefixes = payload
        .get(PayloadField::AllowedPrefixes.as_str())
        .and_then(|p| p.as_array());

    if let Some(n) = payload
        .get(PayloadField::Count.as_str())
        .and_then(|c| c.as_u64())
    {
        if n as usize > MAX_COUNT {
            return Err(GetError::CountExceeded(MAX_COUNT).into_response());
        }
    }
    let count_limit = payload
        .get(PayloadField::Count.as_str())
        .and_then(|c| c.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_COUNT);
    let offset = payload
        .get(PayloadField::Offset.as_str())
        .and_then(|c| c.as_u64())
        .map(|n| n as usize)
        .unwrap_or(0);

    // Capture current time once for TTL expiry checks.
    let query_time = ttl::now_ms();

    // Check collection-level TTL expiry -- O(1), not per-document.
    if let Some(exp) = db.get_ttl_expiry(col_name) {
        if ttl::collection_is_expired(exp, query_time) {
            return Err(GetError::CollectionExpired.into_response());
        }
    }

    // Compute the virtual _expiresAt value once for the whole request.
    let expires_val = db
        .get_ttl_expiry(col_name)
        .map(|ms| Value::String(ttl::ms_to_iso(ms)));

    Ok(GetParams {
        col_name,
        where_clause,
        joins_req,
        sort_specs,
        count_limit,
        offset,
        fields_req,
        excluded_req,
        allowed_prefixes,
        expires_val,
    })
}

/// Fast path: bounded heap sort when there are no joins, no key filter, and no
/// prefix filter. Returns `Some((status, body))` if the path was taken, `None`
/// if the caller should fall through to the general path.
fn run_fast_sort_path(
    db: &engine::Db,
    _payload: &Value,
    params: &GetParams<'_>,
) -> Option<(u16, Value)> {
    let specs = params.sort_specs.clone()?;
    let cap = params.offset + params.count_limit;
    let where_clause_owned = params.where_clause.cloned();

    // Fast path: single numeric sort spec — use lazy byte extraction to avoid
    // full deserialization during the parallel scan phase.
    let top = if specs.len() == 1 {
        let spec = &specs[0];
        let (field, descending) = if let Some(s) = spec.as_str() {
            (s.to_string(), false)
        } else if let Some(obj) = spec.as_object() {
            let f = obj
                .get("field")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string();
            let d = obj
                .get("order")
                .and_then(|o| o.as_str())
                .map(|o| o.eq_ignore_ascii_case("desc"))
                .unwrap_or(false);
            (f, d)
        } else {
            (String::new(), false)
        };

        if !field.is_empty() {
            let raw = db.scan_top_n_raw(
                params.col_name,
                move |_key, doc_bytes| {
                    where_clause_owned
                        .as_ref()
                        .map(|c| query::evaluate_where_msgpack(doc_bytes, c).unwrap_or(false))
                        .unwrap_or(true)
                },
                &field,
                descending,
                cap,
            );
            // scan_top_n_raw returns items sorted ascending by sort_value (smallest
            // first). For descending queries the values were negated, so the order
            // is already correct — largest original value comes first.
            raw
        } else {
            // Field name could not be parsed; fall back to generic path.
            let cmp = make_comparator(specs);
            db.scan_top_n(
                params.col_name,
                move |_key, doc_bytes| {
                    where_clause_owned
                        .as_ref()
                        .map(|c| query::evaluate_where_msgpack(doc_bytes, c).unwrap_or(false))
                        .unwrap_or(true)
                },
                cmp,
                cap,
            )
        }
    } else {
        // Multi-field sort: fall back to the generic Value-based comparator.
        let cmp = make_comparator(specs);
        db.scan_top_n(
            params.col_name,
            move |_key, doc_bytes| {
                where_clause_owned
                    .as_ref()
                    .map(|c| query::evaluate_where_msgpack(doc_bytes, c).unwrap_or(false))
                    .unwrap_or(true)
            },
            cmp,
            cap,
        )
    };

    let array: Vec<Value> = top
        .into_iter()
        .skip(params.offset)
        .filter_map(|(k, doc)| {
            shape_doc(
                doc,
                &k,
                params.fields_req,
                params.excluded_req,
                &[],
                params.expires_val.clone(),
            )
        })
        .collect();

    if array.is_empty() {
        Some(GetError::NoDocumentsFound.into_response())
    } else {
        Some(GetSuccess::Documents(array).into_response())
    }
}

/// Apply prefix gating, cross-collection joins, and WHERE filtering to the raw
/// document list. Returns the surviving `(key, doc, join_aliases)` triples, or
/// `Err((status, body))` if a WHERE evaluation error occurs.
fn filter_and_join_docs(
    db: &engine::Db,
    raw: Vec<(String, Value)>,
    params: &GetParams<'_>,
) -> Result<Vec<(String, Value, Vec<String>)>, (u16, Value)> {
    let mut results: Vec<(String, Value, Vec<String>)> = Vec::with_capacity(raw.len());

    for (key, mut doc) in raw {
        // Prefix gatekeeper (used by scoped auth tokens).
        if let Some(prefixes) = params.allowed_prefixes {
            if !prefixes.is_empty() {
                let allowed = prefixes
                    .iter()
                    .filter_map(|p| p.as_str())
                    .any(|p| key.starts_with(p));
                if !allowed {
                    continue;
                }
            }
        }

        // Cross-collection joins.
        let join_aliases = if let Some(joins) = params.joins_req {
            apply_joins(db, &mut doc, joins)
        } else {
            vec![]
        };

        // WHERE filter (re-applied here when joins are present, since joined fields may be tested).
        if let Some(clause) = params.where_clause {
            match query::evaluate_where(&doc, clause) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => return Err(GetError::WhereEvalError(e.to_string()).into_response()),
            }
        }

        results.push((key, doc, join_aliases));
    }

    Ok(results)
}

/// Sort, paginate, and shape the final result set. Handles the single-key
/// shortcut (no array wrapper, no `_key` field) and the general array response.
fn shape_and_return(
    mut results: Vec<(String, Value, Vec<String>)>,
    payload: &Value,
    params: &GetParams<'_>,
) -> (u16, Value) {
    // Single-key lookup -- return the document directly (no array wrapper, no _key).
    if let Some(Value::String(_)) = payload.get(PayloadField::Keys.as_str()) {
        let (key, doc, aliases) = results.remove(0);
        return match shape_doc(
            doc,
            &key,
            params.fields_req,
            params.excluded_req,
            &aliases,
            params.expires_val.clone(),
        ) {
            None => GetError::NoDocumentsFound.into_response(),
            Some(mut out) => {
                if let Some(obj) = out.as_object_mut() {
                    let _ = obj.remove("_key");
                }
                GetSuccess::Document(out).into_response()
            }
        };
    }

    // Sort.
    if let Some(specs) = params.sort_specs.clone() {
        let cmp = make_comparator(specs);
        results.sort_by(|(_, a, _), (_, b, _)| cmp(a, b));
    }

    // Paginate.
    results.truncate(params.offset + params.count_limit);

    // Shape and return.
    let array: Vec<Value> = results
        .into_iter()
        .skip(params.offset)
        .filter_map(|(k, doc, aliases)| {
            shape_doc(
                doc,
                &k,
                params.fields_req,
                params.excluded_req,
                &aliases,
                params.expires_val.clone(),
            )
        })
        .collect();

    if array.is_empty() {
        return GetError::NoDocumentsFound.into_response();
    }

    GetSuccess::Documents(array).into_response()
}
