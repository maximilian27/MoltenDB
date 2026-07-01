use crate::common::payload_fields::PayloadField;
use crate::handlers::get::types::FetchParams;
use crate::{engine, query};
use serde_json::Value;

/// Returns true if `op` is supported by `evaluate_predicate_msgpack`
/// (i.e. can be evaluated directly on raw MsgPack bytes without full deserialization).
fn is_fast_path_op(op: &str) -> bool {
    matches!(
        op,
        "$eq"
            | "$equals"
            | "$ne"
            | "$notEquals"
            | "$in"
            | "$oneOf"
            | "$nin"
            | "$notIn"
            | "$gt"
            | "$greaterThan"
            | "$gte"
            | "$greaterThanOrEqual"
            | "$lt"
            | "$lessThan"
            | "$lte"
            | "$lessThanOrEqual"
            | "$ct"
            | "$contains"
    )
}

/// Fetch raw documents from the engine based on the request payload.
/// - Single key: returns one document.
/// - Key array: returns a batch.
/// - No keys + where clause (no joins): uses get_filtered for early pruning.
/// - No keys, no where / has joins: full collection scan via get_all.
/// Fetch raw documents from the engine based on the fetch parameters struct.
pub fn fetch_documents(
    db: &engine::Db,
    params: &FetchParams<'_>, // <-- Expecting the new struct reference here
) -> Vec<(String, Value)> {
    // Helper to map allowed_prefixes from Option<&Vec<Value>> to Option<Vec<String>>
    let allowed_prefixes_strings: Option<Vec<&str>> = params
        .allowed_prefixes
        .map(|pfx_vec| pfx_vec.iter().filter_map(|v| v.as_str()).collect());

    match params.payload.get(PayloadField::Keys.as_str()) {
        Some(Value::String(k)) => {
            if let Some(ref prefixes) = allowed_prefixes_strings {
                if !prefixes.iter().any(|p| k.starts_with(p)) {
                    return Vec::new();
                }
            }
            db.get(params.col_name, vec![k.clone()])
                .into_iter()
                .collect()
        }
        Some(Value::Array(arr)) => {
            let ks = arr
                .iter()
                .filter_map(|v| {
                    let s = v.as_str()?;
                    if let Some(ref prefixes) = allowed_prefixes_strings {
                        if !prefixes.iter().any(|p| s.starts_with(p)) {
                            return None;
                        }
                    }
                    Some(s.to_string())
                })
                .collect();
            db.get(params.col_name, ks).into_iter().collect()
        }
        _ => {
            // Full scan -- apply WHERE early when there are no joins
            if let Some(clause) = params.where_clause {
                if !params.has_joins {
                    // Extract all AND-conditions that can be evaluated directly on
                    // raw MsgPack bytes. Returns a non-empty Vec when ALL conditions
                    // in the WHERE clause are fast-path compatible; empty Vec means
                    // at least one condition requires full deserialization (e.g. $or,
                    // $and, or unknown/unsupported operators).
                    let fast_preds = extract_fast_predicates(clause);
                    let clause = clause.clone();
                    let prefixes = allowed_prefixes_strings.clone();

                    return db.get_filtered(
                        params.col_name,
                        move |key, doc_bytes| {
                            if let Some(ref pfxs) = prefixes {
                                if !pfxs.iter().any(|p| key.starts_with(p)) {
                                    return false;
                                }
                            }
                            // Fast path: evaluate all conditions directly on MsgPack bytes.
                            // This covers single-op, multi-op on same field
                            // (e.g. $gte + $lte), and multi-field predicates �
                            // all without full rmp_serde deserialization.
                            if !fast_preds.is_empty() {
                                return fast_preds.iter().all(|(field, op, val)| {
                                    query::evaluate_predicate_msgpack(doc_bytes, field, op, val)
                                        .unwrap_or(false)
                                });
                            }
                            // Full where clause (e.g. $or/$and) — still evaluated on raw bytes.
                            query::evaluate_where_msgpack(doc_bytes, &clause).unwrap_or(false)
                        },
                        0,
                        // When a sort is present the caller must see all matching
                        // documents before sorting, so we cannot short-circuit early.
                        if params.has_sort {
                            None
                        } else {
                            Some(params.offset + params.count_limit)
                        },
                        params.default_order_asc,
                    );
                }
            }

            if let Some(ref prefixes) = allowed_prefixes_strings {
                let pfxs = prefixes.clone();
                return db.get_filtered(
                    params.col_name,
                    move |key, _| pfxs.iter().any(|p| key.starts_with(p)),
                    params.offset,
                    // When a sort is present all matches are needed before sorting.
                    if params.has_sort {
                        None
                    } else {
                        Some(params.count_limit)
                    },
                    params.default_order_asc,
                );
            }

            db.get_all(params.col_name, params.offset, Some(params.count_limit))
        }
    }
}

/// Extract all AND-conditions from a WHERE clause as a flat list of
/// `(field_path, operator, value)` triples that can each be evaluated
/// directly on raw MsgPack bytes via `query::evaluate_predicate_msgpack`.
///
/// Returns a **non-empty** Vec when every condition in the clause is
/// fast-path compatible. Returns an **empty** Vec (fall back to full
/// deserialization) when:
///   - Any top-level key starts with `$` (logical operators: `$or`, `$and`, �)
///   - Any operator is not in the fast-path set (e.g. unknown/custom operators)
///   - The clause is not a plain JSON object
///
/// Examples:
///   `{ "brand": "Apple" }`
///       � `[("brand", "$eq", "Apple")]`
///
///   `{ "price": { "$gte": 1500, "$lte": 2500 } }`
///       � `[("price", "$gte", 1500), ("price", "$lte", 2500)]`
///
///   `{ "in_stock": true, "price": { "$lt": 1000 } }`
///       � `[("in_stock", "$eq", true), ("price", "$lt", 1000)]`
///
///   - Any operator is not in the fast-path set (e.g. unknown/custom operators)
///       � `[]`  (requires full deserialization)
fn extract_fast_predicates(clause: &Value) -> Vec<(String, String, Value)> {
    // Handle array format: [{...}, {...}] — implicit AND; flatten all elements.
    if let Some(arr) = clause.as_array() {
        let mut result = Vec::new();
        for item in arr {
            let preds = extract_fast_predicates(item);
            if preds.is_empty() {
                return Vec::new();
            }
            result.extend(preds);
        }
        return result;
    }

    let obj = match clause.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };

    let mut result = Vec::new();

    for (field, condition) in obj {
        // Logical operators ($or, $and, $not, �) require full deserialization.
        if field.starts_with('$') {
            return Vec::new();
        }

        match condition {
            // Implicit equality: { "field": scalar }
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                result.push((field.clone(), "$eq".to_string(), condition.clone()));
            }
            // Explicit operator(s): { "field": { "$op": value, � } }
            Value::Object(op_obj) => {
                for (op, op_val) in op_obj {
                    if !is_fast_path_op(op) {
                        // Unsupported operator � bail out entirely.
                        return Vec::new();
                    }
                    result.push((field.clone(), op.clone(), op_val.clone()));
                }
            }
            // Null, Array, or nested object � not directly comparable; fall back.
            _ => return Vec::new(),
        }
    }

    result
}
