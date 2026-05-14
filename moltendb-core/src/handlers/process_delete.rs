use serde_json::{Value, json};
use crate::validation;
use crate::engine;
use crate::query;

/// Handle a DELETE request.
///
/// Four modes:
///   - Single key:    { "collection": "users", "keys": "u1" }
///   - Batch keys:    { "collection": "users", "keys": ["u1", "u2"] }
///   - WHERE filter:  { "collection": "users", "where": { "role": { "$eq": "guest" } } }
///   - Drop all:      { "collection": "users", "drop": true }
pub fn process_delete(db: &engine::Db, payload: &Value, max_body_size: usize, max_keys_per_request: usize) -> (u16, Value) {
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    // Only "collection", "keys", "where", "count", and "drop" are valid for a delete request.
    const DELETE_ALLOWED: &[&str] = &["collection", "keys", "where", "count", "drop"];
    if let Err(e) = validation::validate_allowed_properties(payload, DELETE_ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    let col = payload["collection"].as_str().unwrap_or("default");

    // Check for drop: true — this removes the entire collection.
    if payload["drop"].as_bool().unwrap_or(false) {
        return match db.delete_collection(col) {
            Ok(_)  => (200, json!({ "status": "ok", "dropped": true })),
            Err(e) => (500, json!({ "error": "Failed to drop collection", "details": e.to_string(), "statusCode": 500 }))
        };
    }

    // WHERE-based bulk delete — scan with predicate, delete all matches.
    if let Some(clause) = payload.get("where").cloned() {
        let count_limit = payload.get("count").and_then(|c| c.as_u64()).map(|n| n as usize);
        return match db.delete_filtered(col, move |doc| query::evaluate_where(doc, &clause).unwrap_or(false), count_limit) {
            Ok(count) => (200, json!({ "status": "ok", "deleted": count })),
            Err(e)    => (500, json!({ "error": "Failed to delete", "details": e.to_string(), "statusCode": 500 }))
        };
    }

    match payload.get("keys") {
        // Single key delete.
        Some(Value::String(k)) => {
            match db.delete(col, vec![k.to_string()]) {
                Ok(_)  => (200, json!({ "status": "ok", "deleted": 1 })),
                Err(e) => (500, json!({ "error": "Failed to delete key", "details": e.to_string(), "statusCode": 500 }))
            }
        },

        // Batch key delete — collect all keys then delete in one call.
        Some(Value::Array(arr)) => {
            let mut keys = Vec::new();
            for k in arr {
                if let Some(s) = k.as_str() { keys.push(s.to_string()); }
            }
            let count = keys.len();
            match db.delete(col, keys) {
                Ok(_)  => (200, json!({ "status": "ok", "deleted": count })),
                Err(e) => (500, json!({ "error": "Failed to delete batch", "details": e.to_string(), "statusCode": 500 }))
            }
        },
        // Neither keys, where, nor drop:true — invalid request.
        _ => (400, json!({ "error": "Missing 'keys' (string or array), 'where', or 'drop': true", "statusCode": 400 }))
    }
}
