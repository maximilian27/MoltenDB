use crate::engine::DbError;
use serde_json::{Value, Map};
use rmp::decode::read_marker;
use serde::de::Deserialize;

// ─── MsgPack fast-path helpers ───────────────────────────────────────────────
// These functions walk raw MsgPack bytes to evaluate predicates without
// deserializing the whole document into serde_json::Value.

/// Read a MsgPack string at the front of `bytes`, returning the str slice and
/// advancing `bytes` past it. Returns None if the next value is not a string.
fn read_msgpack_str<'a>(bytes: &mut &'a [u8]) -> Option<&'a str> {
    let marker = read_marker(bytes).ok()?;
    let len = match marker {
        rmp::Marker::FixStr(l) => l as usize,
        rmp::Marker::Str8 => {
            if bytes.is_empty() { return None; }
            let l = bytes[0] as usize;
            *bytes = &bytes[1..];
            l
        }
        rmp::Marker::Str16 => {
            if bytes.len() < 2 { return None; }
            let l = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
            *bytes = &bytes[2..];
            l
        }
        rmp::Marker::Str32 => {
            if bytes.len() < 4 { return None; }
            let l = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
            *bytes = &bytes[4..];
            l
        }
        _ => return None,
    };
    if bytes.len() < len { return None; }
    let s = std::str::from_utf8(&bytes[..len]).ok()?;
    *bytes = &bytes[len..];
    Some(s)
}

/// Read the number of entries in a MsgPack map, advancing `bytes` past the
/// map header. Returns None if the next value is not a map.
fn read_msgpack_map_len(bytes: &mut &[u8]) -> Option<u32> {
    let marker = read_marker(bytes).ok()?;
    match marker {
        rmp::Marker::FixMap(l) => Some(l as u32),
        rmp::Marker::Map16 => {
            if bytes.len() < 2 { return None; }
            let l = u16::from_be_bytes([bytes[0], bytes[1]]) as u32;
            *bytes = &bytes[2..];
            Some(l)
        }
        rmp::Marker::Map32 => {
            if bytes.len() < 4 { return None; }
            let l = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            *bytes = &bytes[4..];
            Some(l)
        }
        _ => None,
    }
}

/// Read the number of entries in a MsgPack array, advancing `bytes` past the
/// array header. Returns None if the next value is not an array.
fn read_msgpack_array_len(bytes: &mut &[u8]) -> Option<u32> {
    let marker = read_marker(bytes).ok()?;
    match marker {
        rmp::Marker::FixArray(l) => Some(l as u32),
        rmp::Marker::Array16 => {
            if bytes.len() < 2 { return None; }
            let l = u16::from_be_bytes([bytes[0], bytes[1]]) as u32;
            *bytes = &bytes[2..];
            Some(l)
        }
        rmp::Marker::Array32 => {
            if bytes.len() < 4 { return None; }
            let l = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            *bytes = &bytes[4..];
            Some(l)
        }
        _ => None,
    }
}

/// Skip one MsgPack value, advancing `bytes` past it.
fn skip_value(bytes: &mut &[u8]) {
    let mut de = rmp_serde::Deserializer::new(*bytes);
    if let Ok(_) = serde::de::IgnoredAny::deserialize(&mut de) {
        *bytes = de.into_inner();
    } else {
        *bytes = &[];
    }
}

/// Navigate a dot-notation path inside a MsgPack map, returning a slice
/// starting at the value for the final path component. Returns None if any
/// segment is not found or the intermediate value is not a map.
///
/// `path_parts` must have at least one element.
fn find_msgpack_value<'a>(mut bytes: &'a [u8], path_parts: &[&str]) -> Option<&'a [u8]> {
    for (depth, &segment) in path_parts.iter().enumerate() {
        let map_len = read_msgpack_map_len(&mut bytes)?;
        let mut found = false;
        for _ in 0..map_len {
            let key = read_msgpack_str(&mut bytes)?;
            if key == segment {
                if depth == path_parts.len() - 1 {
                    // This is the final segment — return a slice starting here
                    return Some(bytes);
                }
                // Intermediate segment — recurse into the nested map
                found = true;
                break;
            } else {
                skip_value(&mut bytes);
            }
        }
        if !found && depth < path_parts.len() - 1 {
            return None;
        }
    }
    None
}

/// Read the string value at the current position in `bytes` (case-insensitive
/// comparison helper). Returns None if the value is not a string.
fn read_str_value(bytes: &[u8]) -> Option<String> {
    let mut b = bytes;
    read_msgpack_str(&mut b).map(|s| s.to_lowercase())
}

/// Evaluate a single-field predicate directly against MsgPack bytes.
///
/// Supported operators: `$eq`, `$ne`, `$in`, `$nin`.
/// The field path may use dot-notation (e.g. `"specs.cpu.brand"`).
/// String comparisons are case-insensitive.
///
/// Returns `None` if the operator is not one of the above (caller should fall
/// back to full deserialization).
pub fn evaluate_predicate_msgpack(
    msgpack_bytes: &[u8],
    field_path: &str,
    operator: &str,
    op_value: &Value,
) -> Option<bool> {
    let parts: Vec<&str> = field_path.split('.').collect();
    let value_slice = find_msgpack_value(msgpack_bytes, &parts)?;

    match operator {
        "$eq" | "$equals" => {
            match op_value {
                Value::String(expected) => {
                    let actual = read_str_value(value_slice)?;
                    Some(actual == expected.to_lowercase())
                }
                Value::Number(n) => {
                    // Read numeric value from msgpack
                    let actual = read_msgpack_number(value_slice)?;
                    Some((actual - n.as_f64()?).abs() < f64::EPSILON)
                }
                Value::Bool(b) => {
                    let actual = read_msgpack_bool(value_slice)?;
                    Some(actual == *b)
                }
                _ => None,
            }
        }
        "$ne" | "$notEquals" => {
            match op_value {
                Value::String(expected) => {
                    let actual = read_str_value(value_slice)?;
                    Some(actual != expected.to_lowercase())
                }
                Value::Number(n) => {
                    let actual = read_msgpack_number(value_slice)?;
                    Some((actual - n.as_f64()?).abs() >= f64::EPSILON)
                }
                Value::Bool(b) => {
                    let actual = read_msgpack_bool(value_slice)?;
                    Some(actual != *b)
                }
                _ => None,
            }
        }
        "$in" | "$oneOf" => {
            let allowed = op_value.as_array()?;
            let actual = read_str_value(value_slice);
            Some(allowed.iter().any(|v| match (actual.as_deref(), v) {
                (Some(a), Value::String(b)) => a == b.to_lowercase().as_str(),
                _ => false,
            }))
        }
        "$nin" | "$notIn" => {
            let excluded = op_value.as_array()?;
            let actual = read_str_value(value_slice);
            Some(!excluded.iter().any(|v| match (actual.as_deref(), v) {
                (Some(a), Value::String(b)) => a == b.to_lowercase().as_str(),
                _ => false,
            }))
        }
        "$gt" | "$greaterThan" => {
            let threshold = op_value.as_f64()?;
            let actual = read_msgpack_number(value_slice)?;
            Some(actual > threshold)
        }
        "$gte" | "$greaterThanOrEqual" => {
            let threshold = op_value.as_f64()?;
            let actual = read_msgpack_number(value_slice)?;
            Some(actual >= threshold)
        }
        "$lt" | "$lessThan" => {
            let threshold = op_value.as_f64()?;
            let actual = read_msgpack_number(value_slice)?;
            Some(actual < threshold)
        }
        "$lte" | "$lessThanOrEqual" => {
            let threshold = op_value.as_f64()?;
            let actual = read_msgpack_number(value_slice)?;
            Some(actual <= threshold)
        }
        "$ct" | "$contains" => {
            let needle = op_value.as_str()?.to_lowercase();
            // Try plain string first.
            if let Some(haystack) = read_str_value(value_slice) {
                return Some(haystack.contains(needle.as_str()));
            }
            // Try array: check if any string element contains the needle.
            let mut arr_bytes = value_slice;
            if let Some(arr_len) = read_msgpack_array_len(&mut arr_bytes) {
                for _ in 0..arr_len {
                    if let Some(elem) = read_msgpack_str(&mut arr_bytes) {
                        if elem.to_lowercase().contains(needle.as_str()) {
                            return Some(true);
                        }
                    } else {
                        skip_value(&mut arr_bytes);
                    }
                }
                return Some(false);
            }
            None
        }
        _ => None,
    }
}


fn read_msgpack_number(bytes: &[u8]) -> Option<f64> {
    let mut b = bytes;
    let marker = read_marker(&mut b).ok()?;
    match marker {
        rmp::Marker::FixPos(v) => Some(v as f64),
        rmp::Marker::FixNeg(v) => Some(v as f64),
        rmp::Marker::U8 => { let v = b.first().copied()? as f64; Some(v) }
        rmp::Marker::U16 => {
            if b.len() < 2 { return None; }
            Some(u16::from_be_bytes([b[0], b[1]]) as f64)
        }
        rmp::Marker::U32 => {
            if b.len() < 4 { return None; }
            Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64)
        }
        rmp::Marker::U64 => {
            if b.len() < 8 { return None; }
            Some(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64)
        }
        rmp::Marker::I8 => { let v = b.first().copied()? as i8 as f64; Some(v) }
        rmp::Marker::I16 => {
            if b.len() < 2 { return None; }
            Some(i16::from_be_bytes([b[0], b[1]]) as f64)
        }
        rmp::Marker::I32 => {
            if b.len() < 4 { return None; }
            Some(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64)
        }
        rmp::Marker::I64 => {
            if b.len() < 8 { return None; }
            Some(i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64)
        }
        rmp::Marker::F32 => {
            if b.len() < 4 { return None; }
            Some(f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64)
        }
        rmp::Marker::F64 => {
            if b.len() < 8 { return None; }
            Some(f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        }
        _ => None,
    }
}

/// Extract the `_seq` / `_s` field from a MsgPack document as a u64.
/// Checks the compact storage name `"_s"` first, then falls back to the
/// legacy long name `"_seq"` for documents written before the rename.
/// Returns `u64::MAX` as fallback so docs without a seq field sort last.
pub fn read_msgpack_seq(bytes: &[u8]) -> u64 {
    find_msgpack_value(bytes, &["_s"])
        .or_else(|| find_msgpack_value(bytes, &["_seq"]))
        .and_then(|slice| {
            let mut b = slice;
            let marker = read_marker(&mut b).ok()?;
            match marker {
                rmp::Marker::FixPos(v) => Some(v as u64),
                rmp::Marker::U8 => Some(*b.first()? as u64),
                rmp::Marker::U16 => {
                    if b.len() < 2 { return None; }
                    Some(u16::from_be_bytes([b[0], b[1]]) as u64)
                }
                rmp::Marker::U32 => {
                    if b.len() < 4 { return None; }
                    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64)
                }
                rmp::Marker::U64 => {
                    if b.len() < 8 { return None; }
                    Some(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
                }
                _ => None,
            }
        })
        .unwrap_or(u64::MAX)
}

fn read_msgpack_bool(bytes: &[u8]) -> Option<bool> {
    let mut b = bytes;
    match read_marker(&mut b).ok()? {
        rmp::Marker::True => Some(true),
        rmp::Marker::False => Some(false),
        _ => None,
    }
}

/// Evaluates a simple equality condition directly against MsgPack bytes.
/// Avoids the expensive decoding of the entire document into serde_json::Value.
pub fn evaluate_binary_predicate(
    msgpack_bytes: &[u8],
    target_key: &str,
    target_value: &str
) -> bool {
    evaluate_predicate_msgpack(
        msgpack_bytes,
        target_key,
        "$eq",
        &Value::String(target_value.to_string()),
    ).unwrap_or(false)
}

// ─── Projection / exclusion ──────────────────────────────────────────────────

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

// ─── WHERE evaluation (on decoded Value) ─────────────────────────────────────

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

    #[test]
    fn test_evaluate_predicate_msgpack_eq_ne() {
        let doc = json!({ "brand": "Apple", "price": 999.0 });
        let bytes = rmp_serde::to_vec(&doc).unwrap();

        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "brand", "$eq", &json!("Apple")),
            Some(true)
        );
        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "brand", "$eq", &json!("apple")),
            Some(true)
        );
        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "brand", "$ne", &json!("Intel")),
            Some(true)
        );
        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "brand", "$ne", &json!("Apple")),
            Some(false)
        );
    }

    #[test]
    fn test_evaluate_predicate_msgpack_in_nin() {
        let doc = json!({ "brand": "Dell" });
        let bytes = rmp_serde::to_vec(&doc).unwrap();

        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "brand", "$in", &json!(["Apple", "Dell", "Razer"])),
            Some(true)
        );
        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "brand", "$in", &json!(["Apple", "Razer"])),
            Some(false)
        );
        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "brand", "$nin", &json!(["Framework", "Lenovo"])),
            Some(true)
        );
        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "brand", "$nin", &json!(["Dell", "Lenovo"])),
            Some(false)
        );
    }

    #[test]
    fn test_evaluate_predicate_msgpack_nested() {
        let doc = json!({ "specs": { "cpu": { "brand": "Intel" } } });
        let bytes = rmp_serde::to_vec(&doc).unwrap();

        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "specs.cpu.brand", "$eq", &json!("Intel")),
            Some(true)
        );
        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "specs.cpu.brand", "$ne", &json!("Intel")),
            Some(false)
        );
        assert_eq!(
            evaluate_predicate_msgpack(&bytes, "specs.cpu.brand", "$nin", &json!(["AMD", "Apple"]),),
            Some(true)
        );
    }

}
