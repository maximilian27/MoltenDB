use crate::common::where_operators::WhereOperator;
use crate::engine::DbError;
use rmp::decode::read_marker;
use serde::de::Deserialize;
use serde_json::{Map, Value};

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
            if bytes.is_empty() {
                return None;
            }
            let l = bytes[0] as usize;
            *bytes = &bytes[1..];
            l
        }
        rmp::Marker::Str16 => {
            if bytes.len() < 2 {
                return None;
            }
            let l = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
            *bytes = &bytes[2..];
            l
        }
        rmp::Marker::Str32 => {
            if bytes.len() < 4 {
                return None;
            }
            let l = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
            *bytes = &bytes[4..];
            l
        }
        _ => return None,
    };
    if bytes.len() < len {
        return None;
    }
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
            if bytes.len() < 2 {
                return None;
            }
            let l = u16::from_be_bytes([bytes[0], bytes[1]]) as u32;
            *bytes = &bytes[2..];
            Some(l)
        }
        rmp::Marker::Map32 => {
            if bytes.len() < 4 {
                return None;
            }
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
            if bytes.len() < 2 {
                return None;
            }
            let l = u16::from_be_bytes([bytes[0], bytes[1]]) as u32;
            *bytes = &bytes[2..];
            Some(l)
        }
        rmp::Marker::Array32 => {
            if bytes.len() < 4 {
                return None;
            }
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
///
/// Handles both regular string keys and single-byte negative FixInt token keys
/// (0xe0..=0xff) used for system fields in v1 storage format. Token keys are
/// skipped (they are system fields, never user-queryable by path).
pub fn find_msgpack_value<'a>(mut bytes: &'a [u8], path_parts: &[&str]) -> Option<&'a [u8]> {
    for (depth, &segment) in path_parts.iter().enumerate() {
        let map_len = read_msgpack_map_len(&mut bytes)?;
        let mut found = false;
        for _ in 0..map_len {
            // Peek at the key byte to detect negative FixInt token keys.
            let key_byte = *bytes.first()?;
            if key_byte >= 0xe0 {
                // Single-byte negative FixInt token key — skip it and its value.
                bytes = &bytes[1..];
                skip_value(&mut bytes);
                continue;
            }
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

/// Single-pass multi-field extractor.
///
/// Walks the top-level MsgPack map **once**, collecting the raw value slices for
/// every requested path. Nested paths (e.g. `["specs", "cpu", "ghz"]`) are
/// grouped by their first segment so the sub-map is entered only once per
/// unique prefix, not once per requested field.
///
/// `paths` is a slice of pre-split path segments (same format as `find_msgpack_value`).
/// `out` must be the same length as `paths`; each slot receives `Some(&[u8])` if
/// the field was found, `None` otherwise.
pub fn find_msgpack_values_multi<'a>(
    doc: &'a [u8],
    paths: &[&[&str]],
    out: &mut [Option<&'a [u8]>],
) {
    find_msgpack_values_multi_inner(doc, paths, out, 0);
}

/// Recursive inner implementation — `depth` is the current path segment index.
fn find_msgpack_values_multi_inner<'a>(
    doc: &'a [u8],
    paths: &[&[&str]],
    out: &mut [Option<&'a [u8]>],
    depth: usize,
) {
    let mut bytes = doc;
    let map_len = match read_msgpack_map_len(&mut bytes) {
        Some(l) => l,
        None => return,
    };

    // Track which path indices still need to be resolved at this depth.
    // We stop early once all are found.
    let mut remaining = paths.len();
    for i in 0..paths.len() {
        if out[i].is_some() || paths[i].len() <= depth {
            remaining -= 1;
        }
    }

    for _ in 0..map_len {
        if remaining == 0 {
            break;
        }

        // Skip negative FixInt token keys (system fields).
        let key_byte = match bytes.first() {
            Some(&b) => b,
            None => return,
        };
        if key_byte >= 0xe0 {
            bytes = &bytes[1..];
            skip_value(&mut bytes);
            continue;
        }

        let key = match read_msgpack_str(&mut bytes) {
            Some(k) => k,
            None => return,
        };

        // The value starts here — remember the position before consuming it.
        let value_start = bytes;

        // Collect all path indices whose segment at `depth` matches this key.
        let mut leaf_indices: Vec<usize> = Vec::new();
        let mut nested_indices: Vec<usize> = Vec::new();

        for (i, path) in paths.iter().enumerate() {
            if out[i].is_some() || path.len() <= depth {
                continue;
            }
            if path[depth] == key {
                if depth == path.len() - 1 {
                    leaf_indices.push(i);
                } else {
                    nested_indices.push(i);
                }
            }
        }

        if leaf_indices.is_empty() && nested_indices.is_empty() {
            skip_value(&mut bytes);
            continue;
        }

        // Assign leaf results — all point to the same value slice.
        for i in &leaf_indices {
            out[*i] = Some(value_start);
            remaining -= 1;
        }

        if !nested_indices.is_empty() {
            // Build a sub-slice of paths and out slots for the nested call.
            let sub_paths: Vec<&[&str]> = nested_indices.iter().map(|&i| paths[i]).collect();
            let mut sub_out: Vec<Option<&'a [u8]>> = vec![None; nested_indices.len()];
            find_msgpack_values_multi_inner(value_start, &sub_paths, &mut sub_out, depth + 1);
            for (j, &i) in nested_indices.iter().enumerate() {
                if sub_out[j].is_some() {
                    out[i] = sub_out[j];
                    remaining -= 1;
                }
            }
            // Advance past the nested value.
            skip_value(&mut bytes);
        } else {
            skip_value(&mut bytes);
        }
    }
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

    match WhereOperator::from_str(operator)? {
        WhereOperator::Eq => match op_value {
            Value::String(expected) => {
                let actual = read_str_value(value_slice)?;
                Some(actual == expected.to_lowercase())
            }
            Value::Number(n) => {
                let actual = read_msgpack_number(value_slice)?;
                Some((actual - n.as_f64()?).abs() < f64::EPSILON)
            }
            Value::Bool(b) => {
                let actual = read_msgpack_bool(value_slice)?;
                Some(actual == *b)
            }
            _ => None,
        },
        WhereOperator::NotEq => match op_value {
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
        },
        WhereOperator::In => {
            let allowed = op_value.as_array()?;
            let actual = read_str_value(value_slice);
            Some(allowed.iter().any(|v| match (actual.as_deref(), v) {
                (Some(a), Value::String(b)) => a == b.to_lowercase().as_str(),
                _ => false,
            }))
        }
        WhereOperator::NotIn => {
            let excluded = op_value.as_array()?;
            let actual = read_str_value(value_slice);
            Some(!excluded.iter().any(|v| match (actual.as_deref(), v) {
                (Some(a), Value::String(b)) => a == b.to_lowercase().as_str(),
                _ => false,
            }))
        }
        WhereOperator::Gt => {
            let threshold = op_value.as_f64()?;
            let actual = read_msgpack_number(value_slice)?;
            Some(actual > threshold)
        }
        WhereOperator::Gte => {
            let threshold = op_value.as_f64()?;
            let actual = read_msgpack_number(value_slice)?;
            Some(actual >= threshold)
        }
        WhereOperator::Lt => {
            let threshold = op_value.as_f64()?;
            let actual = read_msgpack_number(value_slice)?;
            Some(actual < threshold)
        }
        WhereOperator::Lte => {
            let threshold = op_value.as_f64()?;
            let actual = read_msgpack_number(value_slice)?;
            Some(actual <= threshold)
        }
        WhereOperator::Contains => {
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
        WhereOperator::Or | WhereOperator::And => None,
    }
}

pub fn read_msgpack_number(bytes: &[u8]) -> Option<f64> {
    let mut b = bytes;
    let marker = read_marker(&mut b).ok()?;
    match marker {
        rmp::Marker::FixPos(v) => Some(v as f64),
        rmp::Marker::FixNeg(v) => Some(v as f64),
        rmp::Marker::U8 => {
            let v = b.first().copied()? as f64;
            Some(v)
        }
        rmp::Marker::U16 => {
            if b.len() < 2 {
                return None;
            }
            Some(u16::from_be_bytes([b[0], b[1]]) as f64)
        }
        rmp::Marker::U32 => {
            if b.len() < 4 {
                return None;
            }
            Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64)
        }
        rmp::Marker::U64 => {
            if b.len() < 8 {
                return None;
            }
            Some(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64)
        }
        rmp::Marker::I8 => {
            let v = b.first().copied()? as i8 as f64;
            Some(v)
        }
        rmp::Marker::I16 => {
            if b.len() < 2 {
                return None;
            }
            Some(i16::from_be_bytes([b[0], b[1]]) as f64)
        }
        rmp::Marker::I32 => {
            if b.len() < 4 {
                return None;
            }
            Some(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64)
        }
        rmp::Marker::I64 => {
            if b.len() < 8 {
                return None;
            }
            Some(i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64)
        }
        rmp::Marker::F32 => {
            if b.len() < 4 {
                return None;
            }
            Some(f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64)
        }
        rmp::Marker::F64 => {
            if b.len() < 8 {
                return None;
            }
            Some(f64::from_be_bytes([
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
            ]))
        }
        _ => None,
    }
}

/// Extract the `_seq` token (-3) from a MsgPack document as a u64.
/// Uses the v1 negative FixInt token key (0xfd = -3).
/// Returns `u64::MAX` as fallback so docs without a seq field sort last.
pub use crate::common::system_field_tokens::read_msgpack_seq_token as read_msgpack_seq;


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
    target_value: &str,
) -> bool {
    evaluate_predicate_msgpack(
        msgpack_bytes,
        target_key,
        WhereOperator::Eq.as_str(),
        &Value::String(target_value.to_string()),
    )
    .unwrap_or(false)
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
    if parts.is_empty() {
        return;
    }
    let key = parts[0].to_string();
    if parts.len() == 1 {
        target.insert(key, value);
    } else {
        let next_target = target
            .entry(key)
            .or_insert_with(|| Value::Object(Map::new()));
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
    if parts.is_empty() {
        return;
    }
    let key = parts[0];
    if parts.len() == 1 {
        target.remove(key);
    } else if let Some(child) = target.get_mut(key) {
        if let Some(child_map) = child.as_object_mut() {
            remove_nested_value(child_map, &parts[1..]);
        }
        if target
            .get(key)
            .and_then(|v| v.as_object())
            .map(|o| o.is_empty())
            .unwrap_or(false)
        {
            target.remove(key);
        }
    }
}

// ─── WHERE evaluation on raw MsgPack bytes ───────────────────────────────────

/// Evaluate a full WHERE clause directly against raw MsgPack bytes.
///
/// Handles all operators including `$or`/`$and` recursively. For leaf predicates
/// it delegates to `evaluate_predicate_msgpack` — zero deserialization for the
/// common case. Returns `None` only when a predicate cannot be evaluated on raw
/// bytes (should not happen with well-formed queries); callers should treat
/// `None` as `false` (exclude the document).
pub fn evaluate_where_msgpack(doc_bytes: &[u8], query: &Value) -> Option<bool> {
    // Handle array format: [{...}, {...}] — implicit AND
    if let Some(arr) = query.as_array() {
        for item in arr {
            if !evaluate_where_msgpack(doc_bytes, item).unwrap_or(false) {
                return Some(false);
            }
        }
        return Some(true);
    }

    let query_obj = query.as_object()?;

    for (key, condition) in query_obj {
        if key == WhereOperator::Or.as_str() {
            let sub_queries = condition.as_array()?;
            let any_passed = sub_queries
                .iter()
                .any(|sub| evaluate_where_msgpack(doc_bytes, sub).unwrap_or(false));
            if !any_passed {
                return Some(false);
            }
            continue;
        }

        if key == WhereOperator::And.as_str() {
            let sub_queries = condition.as_array()?;
            for sub in sub_queries {
                if !evaluate_where_msgpack(doc_bytes, sub).unwrap_or(false) {
                    return Some(false);
                }
            }
            continue;
        }

        // Field-level predicate.
        if !condition.is_object() {
            // Implicit equality: { "field": scalar }
            let result =
                evaluate_predicate_msgpack(doc_bytes, key, "$eq", condition).unwrap_or(false);
            if !result {
                return Some(false);
            }
            continue;
        }

        let cond_obj = condition.as_object()?;
        for (op, op_val) in cond_obj {
            let result = evaluate_predicate_msgpack(doc_bytes, key, op, op_val).unwrap_or(false);
            if !result {
                return Some(false);
            }
        }
    }

    Some(true)
}

// ─── WHERE evaluation (on decoded Value) ─────────────────────────────────────

// Evaluates a WHERE clause against a document.
// Supports $or/$and logical operators and field-level operators: $eq, $ne, $gt, $gte, $lt, $lte,
// $contains/$ct, $in/$oneOf, $nin/$notIn. String comparisons are case-insensitive.
pub fn evaluate_where(doc: &Value, query: &Value) -> Result<bool, DbError> {
    // Handle array format: [{...}, {...}] — implicit AND
    if let Some(arr) = query.as_array() {
        for item in arr {
            if !evaluate_where(doc, item)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }

    let query_obj = match query.as_object() {
        Some(obj) => obj,
        None => return Ok(true),
    };

    for (key, condition) in query_obj {
        if key == WhereOperator::Or.as_str() {
            let sub_queries = match condition.as_array() {
                Some(arr) => arr,
                None => {
                    return Err(DbError::InvalidQuery(format!(
                        "{} expects an array",
                        WhereOperator::Or.as_str()
                    )));
                }
            };
            let mut any_passed = false;
            for sub in sub_queries {
                if evaluate_where(doc, sub)? {
                    any_passed = true;
                    break;
                }
            }
            if !any_passed {
                return Ok(false);
            }
            continue;
        }

        if key == WhereOperator::And.as_str() {
            let sub_queries = match condition.as_array() {
                Some(arr) => arr,
                None => {
                    return Err(DbError::InvalidQuery(format!(
                        "{} expects an array",
                        WhereOperator::And.as_str()
                    )));
                }
            };
            for sub in sub_queries {
                if !evaluate_where(doc, sub)? {
                    return Ok(false);
                }
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
                if !matches {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
            continue;
        }

        let cond_obj = condition.as_object().ok_or_else(|| {
            DbError::InvalidQuery(format!(
                "Field condition for '{}' must be an object or plain value",
                key
            ))
        })?;
        let doc_val_ref = doc_val_opt.as_ref().unwrap_or(&Value::Null);

        for (op, op_val) in cond_obj {
            let parsed_op = WhereOperator::from_str(op.as_str());
            let passed: bool = match parsed_op {
                Some(WhereOperator::Eq) => match (doc_val_ref, op_val) {
                    (Value::String(a), Value::String(b)) => a.to_lowercase() == b.to_lowercase(),
                    _ => doc_val_ref == op_val,
                },
                Some(WhereOperator::NotEq) => match (doc_val_ref, op_val) {
                    (Value::String(a), Value::String(b)) => a.to_lowercase() != b.to_lowercase(),
                    _ => doc_val_ref != op_val,
                },
                Some(WhereOperator::Gt) => {
                    if let (Some(d_num), Some(o_num)) = (doc_val_ref.as_f64(), op_val.as_f64()) {
                        d_num > o_num
                    } else {
                        false
                    }
                }
                Some(WhereOperator::Gte) => {
                    if let (Some(d_num), Some(o_num)) = (doc_val_ref.as_f64(), op_val.as_f64()) {
                        d_num >= o_num
                    } else {
                        false
                    }
                }
                Some(WhereOperator::Lt) => {
                    if let (Some(d_num), Some(o_num)) = (doc_val_ref.as_f64(), op_val.as_f64()) {
                        d_num < o_num
                    } else {
                        false
                    }
                }
                Some(WhereOperator::Lte) => {
                    if let (Some(d_num), Some(o_num)) = (doc_val_ref.as_f64(), op_val.as_f64()) {
                        d_num <= o_num
                    } else {
                        false
                    }
                }
                Some(WhereOperator::Contains) => match doc_val_ref {
                    Value::String(d_str) => {
                        if let Some(o_str) = op_val.as_str() {
                            d_str.to_lowercase().contains(&o_str.to_lowercase())
                        } else {
                            false
                        }
                    }
                    Value::Array(arr) => arr.contains(op_val),
                    _ => false,
                },
                Some(WhereOperator::In) => {
                    if let Some(allowed) = op_val.as_array() {
                        allowed.iter().any(|v| match (doc_val_ref, v) {
                            (Value::String(a), Value::String(b)) => {
                                a.to_lowercase() == b.to_lowercase()
                            }
                            _ => doc_val_ref == v,
                        })
                    } else {
                        return Err(DbError::InvalidQuery(format!("{} expects an array", op)));
                    }
                }
                Some(WhereOperator::NotIn) => {
                    if let Some(excluded) = op_val.as_array() {
                        !excluded.iter().any(|v| match (doc_val_ref, v) {
                            (Value::String(a), Value::String(b)) => {
                                a.to_lowercase() == b.to_lowercase()
                            }
                            _ => doc_val_ref == v,
                        })
                    } else {
                        return Err(DbError::InvalidQuery(format!("{} expects an array", op)));
                    }
                }
                Some(WhereOperator::Or) | Some(WhereOperator::And) | None => {
                    return Err(DbError::InvalidQuery(format!("Unknown operator: {}", op)));
                }
            };

            if !passed {
                return Ok(false);
            }
        }
    }

    Ok(true)
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
