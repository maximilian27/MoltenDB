use super::delete::constants::{DEFAULT_DELETE_COUNT, DELETE_ALLOWED, MAX_DELETE_COUNT};
use super::delete::errors::DeleteError;
use crate::engine;
use crate::query;
use crate::validation;
use serde_json::{json, Value};

/// Handle a DELETE request.
///
/// Four modes:
///   - Single key:    { "collection": "users", "keys": "u1" }
///   - Batch keys:    { "collection": "users", "keys": ["u1", "u2"] }
///   - WHERE filter:  { "collection": "users", "where": { "role": { "$eq": "guest" } } }
///   - Drop all:      { "collection": "users", "drop": true }
pub fn process_delete(
    db: &engine::Db,
    payload: &Value,
    max_body_size: usize,
    max_keys_per_request: usize,
) -> (u16, Value) {
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return DeleteError::ValidationError(e.to_string()).into_response();
    }
    // Only "collection", "keys", "where", "count", and "drop" are valid for a delete request.
    if let Err(e) = validation::validate_allowed_properties(payload, DELETE_ALLOWED) {
        return DeleteError::ValidationError(e.to_string()).into_response();
    }

    let col = payload["collection"].as_str().unwrap_or("default");

    // Check for drop: true — this removes the entire collection.
    if payload["drop"].as_bool().unwrap_or(false) {
        return match db.delete_collection(col) {
            Ok(_) => (200, json!({ "status": "ok", "dropped": true })),
            Err(e) => DeleteError::FailedToDropCollection(e.to_string()).into_response(), // Clean, 1-line exit!
        };
    }

    // WHERE-based bulk delete — scan with predicate, delete all matches.
    if let Some(clause) = payload.get("where").cloned() {
        if let Some(n) = payload.get("count").and_then(|c| c.as_u64()) {
            if n as usize > MAX_DELETE_COUNT {
                return DeleteError::CountExceedsMax(MAX_DELETE_COUNT).into_response();
            }
        }
        let count_limit = Some(
            payload
                .get("count")
                .and_then(|c| c.as_u64())
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_DELETE_COUNT),
        );
        return match db.delete_filtered(
            col,
            move |doc| query::evaluate_where(doc, &clause).unwrap_or(false),
            count_limit,
        ) {
            Ok(count) => (200, json!({ "status": "ok", "deleted": count })),
            Err(e) => DeleteError::FailedToDelete(e.to_string()).into_response(), // Clean, 1-line exit!
        };
    }

    match payload.get("keys") {
        // Single key delete.
        Some(Value::String(k)) => match db.delete(col, vec![k.to_string()]) {
            Ok(_) => (200, json!({ "status": "ok", "deleted": 1 })),
            Err(e) => DeleteError::FailedToDeleteKey(e.to_string()).into_response(), // Clean, 1-line exit!
        },

        // Batch key delete — collect all keys then delete in one call.
        Some(Value::Array(arr)) => {
            let mut keys = Vec::new();
            for k in arr {
                if let Some(s) = k.as_str() {
                    keys.push(s.to_string());
                }
            }
            let count = keys.len();
            match db.delete(col, keys) {
                Ok(_) => (200, json!({ "status": "ok", "deleted": count })),
                Err(e) => DeleteError::FailedToDeleteBatch(e.to_string()).into_response(), // Clean, 1-line exit!
            }
        }
        _ => DeleteError::MissingFields.into_response(),
    }
}
