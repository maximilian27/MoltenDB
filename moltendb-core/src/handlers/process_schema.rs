use serde_json::{Value, json};
use crate::validation;
use crate::engine;

/// Handle a SCHEMA (register/update schema) request.
///
/// Format: { "collection": "users", "schema": { ... }, "ttl": 3600 }
/// `ttl` is optional — sets a default TTL in seconds for all documents in the collection.
pub fn process_schema(db: &engine::Db, payload: &Value, max_body_size: usize, max_keys_per_request: usize) -> (u16, Value) {
    // "collection", "schema", and "ttl" are valid for a schema request.
    const SCHEMA_ALLOWED: &[&str] = &["collection", "schema", "ttl", "maxSize"];
    if let Err(e) = validation::validate_allowed_properties(payload, SCHEMA_ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    let col = match payload["collection"].as_str() {
        Some(c) => c,
        None => return (400, json!({ "error": "Missing 'collection' name", "statusCode": 400 }))
    };

    // At least one of "schema", "ttl", or "maxSize" must be provided.
    if payload.get("schema").is_none() && payload.get("ttl").is_none() && payload.get("maxSize").is_none() {
        return (400, json!({ "error": "At least one of 'schema', 'ttl', or 'maxSize' must be provided", "statusCode": 400 }));
    }

    // Register JSON schema if provided.
    if let Some(schema) = payload.get("schema").cloned() {
        #[cfg(feature = "schema")]
        match db.set_schema(col, schema) {
            Ok(_) => {},
            Err(engine::DbError::SchemaValidationError(msg)) => return (400, json!({ "error": format!("Invalid Schema: {}", msg), "statusCode": 400 })),
            Err(e) => return (500, json!({ "error": "Database error", "details": e.to_string(), "statusCode": 500 }))
        }
        #[cfg(not(feature = "schema"))]
        let _ = schema;
    }

    // Register collection-level TTL default if provided.
    if let Some(ttl_val) = payload.get("ttl") {
        match ttl_val.as_u64() {
            Some(secs) => db.set_ttl_default(col, secs),
            None => return (400, json!({ "error": "'ttl' must be a non-negative integer (seconds)", "statusCode": 400 })),
        }
    }

    // Register collection-level maxSize if provided.
    if let Some(max_val) = payload.get("maxSize") {
        match max_val.as_u64() {
            Some(max) if max > 0 => db.set_max_size(col, max as usize),
            _ => return (400, json!({ "error": "'maxSize' must be a positive integer", "statusCode": 400 })),
        }
    }

    (200, json!({ "status": "ok", "collection": col }))
}
