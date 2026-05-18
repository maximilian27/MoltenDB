use serde_json::Value;
use std::collections::HashMap;
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
) -> HashMap<String, Value> {
    match payload.get("keys") {
        Some(Value::String(k)) => db.get(col_name, vec![k.clone()]),
        Some(Value::Array(arr)) => {
            let ks = arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
            db.get(col_name, ks)
        }
        _ => {
            // Full scan -- apply WHERE early when there are no joins (avoids materialising filtered-out docs).
            if let Some(clause) = where_clause {
                if !has_joins {
                    let clause = clause.clone();
                    return db.get_filtered(
                        col_name,
                        move |doc| query::evaluate_where(doc, &clause).unwrap_or(false),
                        0,
                        Some(offset + count_limit),
                    );
                }
            }
            db.get_all(col_name)
        }
    }
}
