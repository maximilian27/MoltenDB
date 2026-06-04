use super::update::constants::UPDATE_ALLOWED;
use crate::engine;
use crate::handlers::common::errors::{HttpError, ValidationError};
use crate::handlers::update::errors::UpdateError;
use crate::handlers::update::responses::UpdateSuccess;
use crate::validation;
use serde_json::Value;

/// Handle an UPDATE (partial merge) request.
///
/// Merges the provided fields into existing documents without overwriting
/// fields that are not mentioned in the update.
///
/// Format: { "collection": "users", "data": { "u1": { "role": "admin" } } }
pub fn process_update(
    db: &engine::Db,
    payload: &Value,
    max_body_size: usize,
    max_keys_per_request: usize,
) -> (u16, Value) {
    // Only "collection" and "data" are valid for an update/patch request.
    if let Err(e) = validation::validate_allowed_properties(payload, UPDATE_ALLOWED) {
        return ValidationError(e.to_string()).into_response();
    }
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return ValidationError(e.to_string()).into_response();
    }

    let col = payload["collection"].as_str().unwrap_or("default");

    if let Some(data_map) = payload.get("data").and_then(|v| v.as_object()) {
        let mut updated_count = 0;
        for (k, v) in data_map {
            let mut v = v.clone();
            if let Some(obj) = v.as_object_mut() {
                // _v is allowed as an optimistic-lock guard on update.
                // All other _-prefixed fields are reserved and cannot be set by the client.
                if obj.keys().any(|k| k.starts_with('_') && k != "_v") {
                    return UpdateError::ReservedFields.into_response();
                }
            }
            match db.update(col, k, v) {
                Ok(true) => updated_count += 1, // Document found and updated
                Ok(false) => {}                 // Document not found -- skip
                Err(engine::DbError::Conflict) => {
                    return UpdateError::VersionConflict.into_response();
                }
                #[cfg(feature = "schema")]
                Err(engine::DbError::SchemaValidationError(e)) => {
                    return ValidationError(e.to_string()).into_response();
                }
                Err(e) => {
                    return UpdateError::UpdateFailed(e.to_string()).into_response();
                }
            }
        }
        UpdateSuccess::Updated(updated_count).into_response()
    } else {
        UpdateError::MissingDataMap.into_response()
    }
}
