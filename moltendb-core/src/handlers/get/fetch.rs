use serde_json::Value;
use crate::{engine, query};

/// Returns true if `op` is one of the four numeric range operators that can
/// be accelerated via the SIMD scan path.
#[cfg(not(target_arch = "wasm32"))]
fn is_numeric_range_op(op: &str) -> bool {
    matches!(op, "$gt" | "$greaterThan" | "$gte" | "$greaterThanOrEqual"
               | "$lt" | "$lessThan"   | "$lte" | "$lessThanOrEqual")
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
                    // Try to extract a single-field operator predicate that can be
                    // evaluated directly on MsgPack bytes — covers $eq, $ne, $in, $nin
                    // and dot-notation paths (e.g. "specs.cpu.brand").
                    // This avoids full rmp_serde deserialization for every document.
                    let fast_pred = extract_single_field_predicate(clause);

                    // SIMD fast path: numeric range predicate with no prefix filter.
                    // Routes to get_filtered_numeric_simd which batches 4 docs per
                    // f64x4 SIMD comparison instead of evaluating one-by-one.
                    #[cfg(not(target_arch = "wasm32"))]
                    if _allowed_prefixes.is_none() {
                        if let Some((ref field, ref op, ref val)) = fast_pred {
                            if is_numeric_range_op(op) {
                                if let Some(threshold) = val.as_f64() {
                                    return db.get_filtered_numeric_simd(
                                        col_name, field, op, threshold, offset, Some(count_limit),
                                    );
                                }
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
                            // Fast path: evaluate directly on MsgPack bytes
                            if let Some((ref field, ref op, ref val)) = fast_pred {
                                return query::evaluate_predicate_msgpack(doc_bytes, field, op, val)
                                    .unwrap_or(false);
                            }
                            // Fallback: full deserialization (complex / logical queries)
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

/// Try to extract a single-field predicate from a WHERE clause of the form:
///   `{ "field": { "$op": value } }`  or  `{ "field": value }` (implicit $eq)
///
/// Returns `Some((field_path, operator, op_value))` when the clause is a
/// single-field condition with an operator supported by
/// `query::evaluate_predicate_msgpack` ($eq, $ne, $in, $nin).
/// Returns `None` for logical operators ($or/$and), multi-field clauses, or
/// operators that require full deserialization ($gt, $lt, $ct, …).
fn extract_single_field_predicate(clause: &Value) -> Option<(String, String, Value)> {
    let obj = clause.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    let (field, condition) = obj.iter().next()?;
    // Skip logical operators
    if field.starts_with('$') {
        return None;
    }
    match condition {
        // Implicit equality: { "field": "value" }
        Value::String(_) | Value::Number(_) | Value::Bool(_) => {
            Some((field.clone(), "$eq".to_string(), condition.clone()))
        }
        // Explicit operator: { "field": { "$op": value } }
        Value::Object(op_obj) if op_obj.len() == 1 => {
            let (op, op_val) = op_obj.iter().next()?;
            match op.as_str() {
                "$eq" | "$equals" | "$ne" | "$notEquals"
                | "$in" | "$oneOf" | "$nin" | "$notIn"
                | "$gt" | "$greaterThan" | "$gte" | "$greaterThanOrEqual"
                | "$lt" | "$lessThan" | "$lte" | "$lessThanOrEqual" => {
                    Some((field.clone(), op.clone(), op_val.clone()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}
