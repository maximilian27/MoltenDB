use tracing::debug;
use serde_json::{Value, json};
use crate::validation;
use crate::engine;

/// Handle a DELETE request.
///
/// Three modes:
///   - Single key:  { "collection": "users", "keys": "u1" }
///   - Batch keys:  { "collection": "users", "keys": ["u1", "u2"] }
///   - Drop all:    { "collection": "users", "drop": true }
pub fn process_delete(db: &engine::Db, payload: &Value, max_body_size: usize) -> (u16, Value) {
    if let Err(e) = validation::validate_request(payload, max_body_size) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    // Only "collection", "keys", and "drop" are valid for a delete request.
    const DELETE_ALLOWED: &[&str] = &["collection", "keys", "drop"];
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

    match payload.get("keys") {
        // Single key delete.
        Some(Value::String(k)) => {
            // Check collection size for auto-eviction (Hybrid Bitcask).
            if let Ok(count) = db.evict_collection(col, db.hot_threshold) {
                if count > 0 {
                    debug!("❄️  Auto-evicted {} documents from {} to disk", count, col);
                }
            }
            match db.delete(col, k) {
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
            // ── Check collection size for auto-eviction ──────────────────────────────
            // If the collection is getting large, evict old documents to disk.
            // Configurable limit: db.hot_threshold documents per collection.
            if let Ok(count) = db.evict_collection(col, db.hot_threshold) {
                if count > 0 {
                    debug!("❄️  Auto-evicted {} documents from {} to disk", count, col);
                }
            }

            match db.delete_batch(col, keys) {
                Ok(_)  => (200, json!({ "status": "ok", "deleted": count })),
                Err(e) => (500, json!({ "error": "Failed to delete batch", "details": e.to_string(), "statusCode": 500 }))
            }
        },
        // Neither keys nor drop:true — invalid request.
        _ => (400, json!({ "error": "Missing 'keys' (string or array) or 'drop': true", "statusCode": 400 }))
    }
}
