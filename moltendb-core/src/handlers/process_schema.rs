use crate::common::payload_fields::PayloadField;
use crate::engine;
use crate::handlers::common::errors::{OperationError, ValidationError};
use crate::handlers::schema::constants::SCHEMA_ALLOWED;
use crate::handlers::schema::errors::SchemaError;
use crate::handlers::schema::responses::SchemaSuccess;
use crate::validation;
use serde_json::Value;

/// Handle a SCHEMA (register/update schema) request.
///
/// Format: { "collection": "users", "schema": { ... }, "ttl": 3600 }
/// `ttl` is optional — sets a default TTL in seconds for all documents in the collection.
pub fn process_schema(
    db: &engine::Db,
    payload: &Value,
    max_body_size: usize,
    max_keys_per_request: usize,
) -> (u16, Value) {
    // "collection", "schema", and "ttl" are valid for a schema request.
    if let Err(e) = validation::validate_allowed_properties(payload, SCHEMA_ALLOWED) {
        return ValidationError(e.to_string()).into_response();
    }
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return ValidationError(e.to_string()).into_response();
    }

    let col = match payload[PayloadField::Collection.as_str()].as_str() {
        Some(c) => c,
        None => return SchemaError::MissingCollection.into_response(),
    };

    // At least one of "schema", "ttl", or "maxSize" must be provided.
    if payload.get(PayloadField::Schema.as_str()).is_none()
        && payload.get(PayloadField::Ttl.as_str()).is_none()
        && payload.get(PayloadField::MaxSize.as_str()).is_none()
    {
        return SchemaError::MissingSchemaFields.into_response();
    }

    // Register JSON schema if provided.
    if let Some(schema) = payload.get(PayloadField::Schema.as_str()).cloned() {
        #[cfg(feature = "schema")]
        match db.set_schema(col, schema) {
            Ok(_) => {}
            Err(engine::DbError::SchemaValidationError(msg)) => {
                return SchemaError::InvalidSchema(msg).into_response();
            }
            Err(e) => return SchemaError::DatabaseError(e.to_string()).into_response(),
        }
        #[cfg(not(feature = "schema"))]
        let _ = schema;
    }

    // Register collection-level TTL default if provided.
    if let Some(ttl_val) = payload.get(PayloadField::Ttl.as_str()) {
        match ttl_val.as_u64() {
            Some(secs) => db.set_ttl_default(col, secs),
            None => return SchemaError::InvalidTtl.into_response(),
        }
    }

    // Register collection-level maxSize if provided.
    if let Some(max_val) = payload.get(PayloadField::MaxSize.as_str()) {
        match max_val.as_u64() {
            Some(max) if max > 0 => db.set_max_size(col, max as usize),
            _ => return SchemaError::InvalidMaxSize.into_response(),
        }
    }

    SchemaSuccess::Updated(col.to_string()).into_response()
}
