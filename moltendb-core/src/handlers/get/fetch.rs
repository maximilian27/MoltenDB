use serde_json::Value;
use crate::{engine, query};

/// Returns true if `op` is one of the four numeric range operators that can
/// be accelerated via the SIMD scan path.
#[cfg(not(target_arch = "wasm32"))]
fn is_numeric_range_op(op: &str) -> bool {
    matches!(op, "$gt" | "$greaterThan" | "$gte" | "$greaterThanOrEqual"
               | "$lt" | "$lessThan"   | "$lte" | "$lessThanOrEqual")
}

/// Returns true if `op` is supported by `evaluate_predicate_msgpack`
/// (i.e. can be evaluated directly on raw MsgPack bytes without full deserialization).
fn is_fast_path_op(op: &str) -> bool {
    matches!(op,
        "$eq" | "$equals" | "$ne" | "$notEquals"
        | "$in" | "$oneOf" | "$nin" | "$notIn"
        | "$gt" | "$greaterThan" | "$gte" | "$greaterThanOrEqual"
        | "$lt" | "$lessThan"   | "$lte" | "$lessThanOrEqual"
    )
}

/// Fetch raw documents from the engine based on the request payload.
/// - Single key: returns one document.
/// - Key array: returns a batch.
/// - No keys + where clause (no joins): uses get_filtered for early pruning.
/// - No keys, no where / has joins: full collection scan via get_all.
pub fn fetch_documents(
    db: &engine::Db,
    col_name: &str,
    payload: &Value,
    where_clause: Option<&Value>,
    has_joins: bool,
    offset: usize,
    count_limit: usize,
    _allowed_prefixes: Option<&[String]>,
) -> Vec<(String, Value)> {
    match payload.get("keys") {
        Some(Value::String(k)) => {
            if let Some(prefixes) = _allowed_prefixes {
                if !prefixes.iter().any(|p| k.starts_with(p)) {
                    return Vec::new();
                }
            }
            db.get(col_name, vec![k.clone()]).into_iter().collect()
        }
        Some(Value::Array(arr)) => {
            let ks = arr.iter().filter_map(|v| {
                let s = v.as_str()?;
                if let Some(prefixes) = _allowed_prefixes {
                    if !prefixes.iter().any(|p| s.starts_with(p)) {
                        return None;
                    }
                }
                Some(s.to_string())
            }).collect();
            db.get(col_name, ks).into_iter().collect()
        }
        _ => {
            // Full scan -- apply WHERE early when there are no joins (avoids materialising filtered-out docs).
            if let Some(clause) = where_clause {
                if !has_joins {
                    // Extract all AND-conditions that can be evaluated directly on
                    // raw MsgPack bytes. Returns a non-empty Vec when ALL conditions
                    // in the WHERE clause are fast-path compatible; empty Vec means
                    // at least one condition requires full deserialization (e.g. $or,
                    // $and, or unknown operators).
                    let fast_preds = extract_fast_predicates(clause);

                    // SIMD fast path: single numeric range predicate with no prefix
                    // filter. Routes to get_filtered_numeric_simd which batches 4
                    // docs per f64x4 SIMD comparison instead of evaluating one-by-one.
                    #[cfg(not(target_arch = "wasm32"))]
                    if _allowed_prefixes.is_none() && fast_preds.len() == 1 {
                        let (ref field, ref op, ref val) = fast_preds[0];
                        if is_numeric_range_op(op) {
                            if let Some(threshold) = val.as_f64() {
                                return db.get_filtered_numeric_simd(
                                    col_name, field, op, threshold, offset, Some(count_limit),
                                );
                            }
                        }
                    }

                    let clause = clause.clone();
                    let prefixes = _allowed_prefixes.map(|p| p.to_vec());
                    return db.get_filtered(
                        col_name,
                        move |key, doc_bytes| {
                            if let Some(ref pfxs) = prefixes {
                                if !pfxs.iter().any(|p| key.starts_with(p)) {
                                    return false;
                                }
                            }
                            // Fast path: evaluate all conditions directly on MsgPack bytes.
                            // This covers single-op, multi-op on same field
                            // (e.g. $gte + $lte), and multi-field predicates —
                            // all without full rmp_serde deserialization.
                            if !fast_preds.is_empty() {
                                return fast_preds.iter().all(|(field, op, val)| {
                                    query::evaluate_predicate_msgpack(doc_bytes, field, op, val)
                                        .unwrap_or(false)
                                });
                            }
                            // Fallback: full deserialization (logical operators, $ct, etc.)
                            let doc: Value = match rmp_serde::from_slice(doc_bytes) {
                                Ok(d) => d,
                                Err(_) => return false,
                            };
                            query::evaluate_where(&doc, &clause).unwrap_or(false)
                        },
                        0,
                        Some(offset + count_limit),
                    );
                }
            }

            // Apply prefix gating even for get_all, or use get_all with offset/limit
            if let Some(prefixes) = _allowed_prefixes {
                let pfxs = prefixes.to_vec();
                return db.get_filtered(
                    col_name,
                    move |key, _| {
                        pfxs.iter().any(|p| key.starts_with(p))
                    },
                    offset,
                    Some(count_limit),
                );
            }

            db.get_all(col_name, offset, Some(count_limit))
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
///   - Any top-level key starts with `$` (logical operators: `$or`, `$and`, …)
///   - Any operator is not in the fast-path set (e.g. `$ct`, `$contains`)
///   - The clause is not a plain JSON object
///
/// Examples:
///   `{ "brand": "Apple" }`
///       → `[("brand", "$eq", "Apple")]`
///
///   `{ "price": { "$gte": 1500, "$lte": 2500 } }`
///       → `[("price", "$gte", 1500), ("price", "$lte", 2500)]`
///
///   `{ "in_stock": true, "price": { "$lt": 1000 } }`
///       → `[("in_stock", "$eq", true), ("price", "$lt", 1000)]`
///
///   - Any operator is not in the fast-path set (e.g. `$ct`, `$contains`)
///       → `[]`  (requires full deserialization)
fn extract_fast_predicates(clause: &Value) -> Vec<(String, String, Value)> {
    let obj = match clause.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };

    let mut result = Vec::new();

    for (field, condition) in obj {
        // Logical operators ($or, $and, $not, …) require full deserialization.
        if field.starts_with('$') {
            return Vec::new();
        }

        match condition {
            // Implicit equality: { "field": scalar }
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                result.push((field.clone(), "$eq".to_string(), condition.clone()));
            }
            // Explicit operator(s): { "field": { "$op": value, … } }
            Value::Object(op_obj) => {
                for (op, op_val) in op_obj {
                    if !is_fast_path_op(op) {
                        // Unsupported operator (e.g. $ct) — bail out entirely.
                        return Vec::new();
                    }
                    result.push((field.clone(), op.clone(), op_val.clone()));
                }
            }
            // Null, Array, or nested object — not directly comparable; fall back.
            _ => return Vec::new(),
        }
    }

    result
}
