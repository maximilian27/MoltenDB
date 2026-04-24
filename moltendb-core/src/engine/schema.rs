use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use crate::engine::types::{DbError, LogEntry};
use crate::engine::storage::StorageBackend;
use tokio::sync::broadcast;
use jsonschema::Validator;

/// Register a JSON schema for a collection.
pub fn set_schema(
    schemas: &DashMap<String, Arc<(Value, Validator)>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &broadcast::Sender<String>,
    collection: &str,
    schema: Value,
) -> Result<(), DbError> {
    // 1. Validate that the schema itself is valid.
    let validator = jsonschema::validator_for(&schema)
        .map_err(|e| DbError::SchemaValidationError(format!("Invalid schema: {}", e)))?;

    // 2. Persist to WAL.
    storage.write_entry(&LogEntry::new(
        "SCHEMA".to_string(),
        collection.to_string(),
        "".to_string(),
        schema.clone(),
    ))?;

    // 3. Update in-memory state.
    schemas.insert(collection.to_string(), Arc::new((schema.clone(), validator)));

    // 4. Notify subscribers.
    let event = serde_json::json!({
        "event": "SCHEMA_SET",
        "collection": collection,
        "schema": schema
    });
    let _ = tx.send(event.to_string());

    Ok(())
}

/// Validate a document against a collection's schema (if one exists).
pub fn validate_document(
    schemas: &DashMap<String, Arc<(Value, Validator)>>,
    collection: &str,
    document: &Value,
) -> Result<(), DbError> {
    if let Some(entry) = schemas.get(collection) {
        let validator = &entry.1;
        if !validator.is_valid(document) {
            // If we want more detailed errors, we can use validate()
            // but is_valid() is the fastest path.
            // For now, let's keep it simple and fast.
            return Err(DbError::SchemaValidationError("Document does not conform to schema".to_string()));
        }
    }
    Ok(())
}
