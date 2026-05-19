use serde_json::Value;
use crate::{engine, query};

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
                    // Extract simple $eq condition for binary evaluation
                    let mut simple_match = None;
                    if let Some(obj) = clause.as_object() {
                        if obj.len() == 1 {
                            let (k, v) = obj.iter().next().unwrap();
                            if !k.starts_with('$') {
                                if let Some(v_str) = v.as_str() {
                                    simple_match = Some((k.clone(), v_str.to_string()));
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
                            if let Some((ref k, ref v)) = simple_match {
                                if query::evaluate_binary_predicate(doc_bytes, k, v) {
                                    return true;
                                }
                                return false;
                            }
                            
                            // fallback
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
                    Some(count_limit), // we want exactly up to count_limit after offset
                ); // wait, if we use get_filtered we need to fetch offset + count_limit in get_filtered because offset skips there.
            }
            
            db.get_all(col_name, offset, Some(count_limit))
        }
    }
}
