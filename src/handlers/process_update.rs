use serde_json::{Value, json};
use crate::{engine, validation};

/// Handle an UPDATE (partial merge) request.
///
/// Merges the provided fields into existing documents without overwriting
/// fields that are not mentioned in the update.
///
/// Format: { "collection": "users", "data": { "u1": { "role": "admin" } } }
pub fn process_update(db: &engine::Db, payload: &Value) -> (u16, Value) {
    // Only "collection" and "data" are valid for an update/patch request.
    const UPDATE_ALLOWED: &[&str] = &["collection", "data"];
    if let Err(e) = validation::validate_allowed_properties(payload, UPDATE_ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    if let Err(e) = validation::validate_request(payload) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    let col = payload["collection"].as_str().unwrap_or("default");

    if let Some(data_map) = payload.get("data").and_then(|v| v.as_object()) {
        let mut updated_count = 0;
        for (k, v) in data_map {
            match db.update(col, k, v.clone()) {
                Ok(true)  => updated_count += 1,  // Document found and updated
                Ok(false) => {},                   // Document not found — skip
                Err(e) => return (500, json!({ "error": "Database update failed", "details": e.to_string(), "statusCode": 500 }))
            }
        }
        (200, json!({ "status": "ok", "updated": updated_count }))
    } else {
        (400, json!({ "error": "Missing 'data' map", "statusCode": 400 }))
    }
}