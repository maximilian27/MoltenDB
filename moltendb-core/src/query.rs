use crate::engine::DbError;
use serde_json::{Value, Map};
use rmp::decode::read_marker;
use serde::de::Deserialize;

/// Evaluates a simple equality condition directly against MsgPack bytes.
/// Avoids the expensive decoding of the entire document into serde_json::Value.
pub fn evaluate_binary_predicate(
    msgpack_bytes: &[u8],
    target_key: &str,
    target_value: &str
) -> bool {
    let mut bytes = msgpack_bytes;
    
    // Read the map marker
    let marker = match read_marker(&mut bytes) {
        Ok(m) => m,
        Err(_) => return false,
    };
    
    let len = match marker {
        rmp::Marker::Map16 => {
            let mut len_bytes = [0u8; 2];
            if bytes.len() < 2 { return false; }
            len_bytes.copy_from_slice(&bytes[..2]);
            bytes = &bytes[2..];
            u16::from_be_bytes(len_bytes) as u32
        }
        rmp::Marker::Map32 => {
            let mut len_bytes = [0u8; 4];
            if bytes.len() < 4 { return false; }
            len_bytes.copy_from_slice(&bytes[..4]);
            bytes = &bytes[4..];
            u32::from_be_bytes(len_bytes)
        }
        rmp::Marker::FixMap(l) => l as u32,
        _ => return false,
    };
    
    for _ in 0..len {
        // Read key
        let key_marker = match read_marker(&mut bytes) {
            Ok(m) => m,
            Err(_) => return false,
        };
        
        let key_len = match key_marker {
            rmp::Marker::FixStr(l) => l as u32,
            rmp::Marker::Str8 => {
                if bytes.is_empty() { return false; }
                let l = bytes[0] as u32;
                bytes = &bytes[1..];
                l
            }
            rmp::Marker::Str16 => {
                let mut len_bytes = [0u8; 2];
                if bytes.len() < 2 { return false; }
                len_bytes.copy_from_slice(&bytes[..2]);
                bytes = &bytes[2..];
                u16::from_be_bytes(len_bytes) as u32
            }
            rmp::Marker::Str32 => {
                let mut len_bytes = [0u8; 4];
                if bytes.len() < 4 { return false; }
                len_bytes.copy_from_slice(&bytes[..4]);
                bytes = &bytes[4..];
                u32::from_be_bytes(len_bytes)
            }
            _ => {
                // If key is not a string, skip it
                skip_value(&mut bytes);
                continue;
            }
        };
        
        if bytes.len() < key_len as usize { return false; }
        let key = std::str::from_utf8(&bytes[..key_len as usize]).unwrap_or("");
        bytes = &bytes[key_len as usize..];
        
        if key == target_key {
            // Check value
            let val_marker = match read_marker(&mut bytes) {
                Ok(m) => m,
                Err(_) => return false,
            };
            
            let val_len = match val_marker {
                rmp::Marker::FixStr(l) => l as u32,
                rmp::Marker::Str8 => {
                    if bytes.is_empty() { return false; }
                    let l = bytes[0] as u32;
                    bytes = &bytes[1..];
                    l
                }
                rmp::Marker::Str16 => {
                    let mut len_bytes = [0u8; 2];
                    if bytes.len() < 2 { return false; }
                    len_bytes.copy_from_slice(&bytes[..2]);
                    bytes = &bytes[2..];
                    u16::from_be_bytes(len_bytes) as u32
                }
                rmp::Marker::Str32 => {
                    let mut len_bytes = [0u8; 4];
                    if bytes.len() < 4 { return false; }
                    len_bytes.copy_from_slice(&bytes[..4]);
                    bytes = &bytes[4..];
                    u32::from_be_bytes(len_bytes)
                }
                _ => return false,
            };
            
            if bytes.len() < val_len as usize { return false; }
            let val = std::str::from_utf8(&bytes[..val_len as usize]).unwrap_or("");
            return val == target_value;
        } else {
            // Skip value
            skip_value(&mut bytes);
        }
    }
    
    false
}

fn skip_value(bytes: &mut &[u8]) {
    let mut de = rmp_serde::Deserializer::new(*bytes);
    if let Ok(_) = serde::de::IgnoredAny::deserialize(&mut de) {
        *bytes = de.into_inner();
    } else {
        *bytes = &[];
    }
}

// Returns a new document containing only the requested dot-notation fields.
pub fn project(doc: &Value, fields: &[Value]) -> Value {
    let mut filtered_doc = Map::new();
    for field in fields {
        if let Some(field_path) = field.as_str() {
            let parts: Vec<&str> = field_path.split('.').collect();
            if let Some(val) = get_nested_value(doc, &parts) {
                insert_nested_value(&mut filtered_doc, &parts, val);
            }
        }
    }
    Value::Object(filtered_doc)
}

// Walks a dot-notation path and returns the value at that location, if it exists.
pub fn get_nested_value(doc: &Value, parts: &[&str]) -> Option<Value> {
    let mut current = doc;
    for part in parts {
        if let Some(v) = current.get(*part) {
            current = v;
        } else {
            return None;
        }
    }
    Some(current.clone())
}

fn insert_nested_value(target: &mut Map<String, Value>, parts: &[&str], value: Value) {
    if parts.is_empty() { return; }
    let key = parts[0].to_string();
    if parts.len() == 1 {
        target.insert(key, value);
    } else {
        let next_target = target.entry(key).or_insert_with(|| Value::Object(Map::new()));
        if let Some(next_map) = next_target.as_object_mut() {
            insert_nested_value(next_map, &parts[1..], value);
        }
    }
}

// Returns a copy of the document with the specified dot-notation fields removed.
pub fn exclude(doc: &Value, fields: &[Value]) -> Value {
    let mut result = match doc.as_object() {
        Some(obj) => obj.clone(),
        None => return doc.clone(),
    };
    for field in fields {
        if let Some(field_path) = field.as_str() {
            let parts: Vec<&str> = field_path.split('.').collect();
            remove_nested_value(&mut result, &parts);
        }
    }
    Value::Object(result)
}

fn remove_nested_value(target: &mut Map<String, Value>, parts: &[&str]) {
    if parts.is_empty() { return; }
    let key = parts[0];
    if parts.len() == 1 {
        target.remove(key);
    } else if let Some(child) = target.get_mut(key) {
        if let Some(child_map) = child.as_object_mut() {
            remove_nested_value(child_map, &parts[1..]);
        }
        if target.get(key).and_then(|v| v.as_object()).map(|o| o.is_empty()).unwrap_or(false) {
            target.remove(key);
        }
    }
}

// Evaluates a WHERE clause against a document.
// Supports $or/$and logical operators and field-level operators: $eq, $ne, $gt, $gte, $lt, $lte,
// $contains/$ct, $in/$oneOf, $nin/$notIn. String comparisons are case-insensitive.
pub fn evaluate_where(doc: &Value, query: &Value) -> Result<bool, DbError> {
    let query_obj = match query.as_object() {
        Some(obj) => obj,
        None => return Ok(true),
    };

    for (key, condition) in query_obj {
        if key == "$or" {
            let sub_queries = match condition.as_array() {
                Some(arr) => arr,
                None => return Err(DbError::InvalidQuery("$or expects an array".to_string())),
            };
            let mut any_passed = false;
            for sub in sub_queries {
                if evaluate_where(doc, sub)? {
                    any_passed = true;
                    break;
                }
            }
            if !any_passed { return Ok(false); }
            continue;
        }

        if key == "$and" {
            let sub_queries = match condition.as_array() {
                Some(arr) => arr,
                None => return Err(DbError::InvalidQuery("$and expects an array".to_string())),
            };
            for sub in sub_queries {
                if !evaluate_where(doc, sub)? { return Ok(false); }
            }
            continue;
        }

        let parts: Vec<&str> = key.split('.').collect();
        let doc_val_opt = get_nested_value(doc, &parts);

        if !condition.is_object() {
            if let Some(dv) = &doc_val_opt {
                let matches = match (dv, condition) {
                    (Value::String(a), Value::String(b)) => a.to_lowercase() == b.to_lowercase(),
                    _ => dv == condition,
                };
                if !matches { return Ok(false); }
            } else {
                return Ok(false);
            }
            continue;
        }

        let cond_obj = condition.as_object().ok_or_else(|| {
            DbError::InvalidQuery(format!("Field condition for '{}' must be an object or plain value", key))
        })?;
        let doc_val_ref = doc_val_opt.as_ref().unwrap_or(&Value::Null);

        for (op, op_val) in cond_obj {
            let passed: bool = match op.as_str() {
                "$eq" | "$equals" => match (doc_val_ref, op_val) {
                    (Value::String(a), Value::String(b)) => a.to_lowercase() == b.to_lowercase(),
                    _ => doc_val_ref == op_val,
                },
                "$ne" | "$notEquals" => match (doc_val_ref, op_val) {
                    (Value::String(a), Value::String(b)) => a.to_lowercase() != b.to_lowercase(),
                    _ => doc_val_ref != op_val,
                },
                "$gt" | "$greaterThan" | "$gte" | "$lt" | "$lessThan" | "$lte" => {
                    if let (Some(d_num), Some(o_num)) = (doc_val_ref.as_f64(), op_val.as_f64()) {
                        match op.as_str() {
                            "$gt" | "$greaterThan" => d_num > o_num,
                            "$gte"                 => d_num >= o_num,
                            "$lt" | "$lessThan"    => d_num < o_num,
                            "$lte"                 => d_num <= o_num,
                            _ => false,
                        }
                    } else {
                        false
                    }
                },
                "$contains" | "$ct" => {
                    match doc_val_ref {
                        Value::String(d_str) => {
                            if let Some(o_str) = op_val.as_str() {
                                d_str.to_lowercase().contains(&o_str.to_lowercase())
                            } else {
                                false
                            }
                        }
                        Value::Array(arr) => arr.contains(op_val),
                        _ => false,
                    }
                },
                "$in" | "$oneOf" => {
                    if let Some(allowed) = op_val.as_array() {
                        allowed.iter().any(|v| match (doc_val_ref, v) {
                            (Value::String(a), Value::String(b)) => a.to_lowercase() == b.to_lowercase(),
                            _ => doc_val_ref == v,
                        })
                    } else {
                        return Err(DbError::InvalidQuery(format!("{} expects an array", op)));
                    }
                },
                "$nin" | "$notIn" => {
                    if let Some(excluded) = op_val.as_array() {
                        !excluded.iter().any(|v| match (doc_val_ref, v) {
                            (Value::String(a), Value::String(b)) => a.to_lowercase() == b.to_lowercase(),
                            _ => doc_val_ref == v,
                        })
                    } else {
                        return Err(DbError::InvalidQuery(format!("{} expects an array", op)));
                    }
                },
                _ => return Err(DbError::InvalidQuery(format!("Unknown operator: {}", op))),
            };

            if !passed { return Ok(false); }
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_evaluate_where_basic() {
        let doc = json!({ "name": "Alice", "age": 30 });
        assert!(evaluate_where(&doc, &json!({ "name": "Alice" })).unwrap());
        assert!(!evaluate_where(&doc, &json!({ "name": "Bob" })).unwrap());
        assert!(evaluate_where(&doc, &json!({ "name": { "$eq": "Alice" } })).unwrap());
        assert!(evaluate_where(&doc, &json!({ "name": "alice" })).unwrap());
    }

    #[test]
    fn test_evaluate_where_numeric() {
        let doc = json!({ "age": 30 });
        assert!(evaluate_where(&doc, &json!({ "age": { "$gt": 20 } })).unwrap());
        assert!(evaluate_where(&doc, &json!({ "age": { "$gte": 30 } })).unwrap());
        assert!(evaluate_where(&doc, &json!({ "age": { "$lt": 40 } })).unwrap());
        assert!(evaluate_where(&doc, &json!({ "age": { "$lte": 30 } })).unwrap());
        assert!(!evaluate_where(&doc, &json!({ "age": { "$gt": 30 } })).unwrap());
    }

    #[test]
    fn test_evaluate_where_invalid_ops() {
        let doc = json!({ "name": "Alice" });
        let res = evaluate_where(&doc, &json!({ "name": { "$invalid": "val" } }));
        assert!(res.is_err());
        if let Err(DbError::InvalidQuery(msg)) = res {
            assert!(msg.contains("Unknown operator"));
        } else {
            panic!("Expected InvalidQuery error");
        }
    }

    #[test]
    fn test_evaluate_where_logical() {
        let doc = json!({ "name": "Alice", "age": 30 });
        assert!(evaluate_where(&doc, &json!({ "$or": [{ "name": "Alice" }, { "name": "Bob" }] })).unwrap());
        assert!(evaluate_where(&doc, &json!({ "$or": [{ "name": "Bob" }, { "age": 30 }] })).unwrap());
        assert!(!evaluate_where(&doc, &json!({ "$or": [{ "name": "Bob" }, { "age": 20 }] })).unwrap());
        assert!(evaluate_where(&doc, &json!({ "$and": [{ "name": "Alice" }, { "age": 30 }] })).unwrap());
        assert!(!evaluate_where(&doc, &json!({ "$and": [{ "name": "Alice" }, { "age": 20 }] })).unwrap());
    }

    #[test]
    fn test_evaluate_where_in_nin() {
        let doc = json!({ "role": "admin" });
        assert!(evaluate_where(&doc, &json!({ "role": { "$in": ["admin", "user"] } })).unwrap());
        assert!(!evaluate_where(&doc, &json!({ "role": { "$in": ["guest", "user"] } })).unwrap());
        assert!(evaluate_where(&doc, &json!({ "role": { "$nin": ["guest", "user"] } })).unwrap());
        assert!(!evaluate_where(&doc, &json!({ "role": { "$nin": ["admin", "user"] } })).unwrap());
    }
}
