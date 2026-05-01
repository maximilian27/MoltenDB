use serde_json::{Value, json};
use crate::validation;
use crate::engine;

/// Handle a SCHEMA (register/update schema) request.
///
/// Format: { "collection": "users", "schema": { "type": "object", "properties": { ... } } }
pub fn process_schema(db: &engine::Db, payload: &Value, max_body_size: usize, max_keys_per_request: usize) -> (u16, Value) {
    // Only "collection" and "schema" are valid for a schema request.
    const SCHEMA_ALLOWED: &[&str] = &["collection", "schema"];
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

    let schema = match payload.get("schema") {
        Some(s) => s.clone(),
        None => return (400, json!({ "error": "Missing 'schema' JSON", "statusCode": 400 }))
    };

    match db.set_schema(col, schema) {
        Ok(_) => (200, json!({ "status": "ok", "collection": col })),
        Err(engine::DbError::SchemaValidationError(msg)) => (400, json!({ "error": format!("Invalid Schema: {}", msg), "statusCode": 400 })),
        Err(e) => (500, json!({ "error": "Database error", "details": e.to_string(), "statusCode": 500 }))
    }
}
