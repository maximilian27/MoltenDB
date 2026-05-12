// ─── operations/update.rs ─────────────────────────────────────────────────────
// Update operation: update (partial patch).
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::debug;
use super::common::now_iso;
use super::super::StorageBackend;
use super::super::types::{DbError, LogEntry};

/// Parameters for the [`update`] operation.
///
/// Grouping these into a struct keeps the function signature within Clippy's
/// argument-count limit and makes call sites more readable.
pub struct UpdateParams<'a> {
    pub state: &'a DashMap<String, DashMap<String, Box<[u8]>>>,
    pub storage: &'a Arc<dyn StorageBackend>,
    pub tx: &'a tokio::sync::broadcast::Sender<String>,
    #[cfg(feature = "schema")]
    pub schemas: &'a DashMap<String, Arc<(Value, jsonschema::Validator)>>,
    pub collection: &'a str,
    pub key: &'a str,
    pub updates: Value,
}

/// Partially update (merge) a single document with new field values.
///
/// This is a "patch" operation — only the fields present in `updates` are
/// changed; all other fields in the existing document are preserved.
///
/// Returns `Ok(true)` if the document was found and updated,
/// `Ok(false)` if the document doesn't exist (no-op).
///
/// Example: document { name: "Alice", role: "user" } + update { role: "admin" }
///          → result: { name: "Alice", role: "admin" }
pub fn update(params: UpdateParams<'_>) -> Result<bool, DbError> {
    let UpdateParams {
        state,
        storage,
        tx,
        #[cfg(feature = "schema")]
        schemas,
        collection,
        key,
        updates,
    } = params;
    // TX_BEGIN: Start a transaction for the update.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    if let Some(col) = state.get(collection)
        && let Some(doc) = col.get(key).and_then(|b| rmp_serde::from_slice::<Value>(&b).ok()) {
            let mut doc = doc;

            // Step 1: Merge the update fields into the existing document.
            // Only top-level fields are merged — nested objects are replaced,
            // not recursively merged.
            if let Some(update_obj) = updates.as_object() {
                // If the caller provides a "_v" field in the update, it acts as a guard.
                // If the current version is not equal to this guard, we return Conflict.
                let existing_v = doc.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(guard_v) = update_obj.get("_v").and_then(|v| v.as_u64())
                    && guard_v != existing_v {
                        debug!("⚡ Conflict error: {}/{} update guard _v={} != stored _v={}", collection, key, guard_v, existing_v);
                        return Err(DbError::Conflict);
                    }

                if let Some(doc_obj) = doc.as_object_mut() {
                    for (k, v) in update_obj {
                        // _v and createdAt are managed exclusively by the engine.
                        // Callers cannot set them directly — silently skip if present.
                        if k == "_v" || k == "createdAt" { continue; }
                        doc_obj.insert(k.clone(), v.clone());
                    }
                    // Bump the version counter on every update.
                    doc_obj.insert("_v".to_string(), serde_json::json!(existing_v + 1));
                    // Stamp the modification time. createdAt is already in the
                    // document and is intentionally left untouched.
                    doc_obj.insert("modifiedAt".to_string(), serde_json::json!(now_iso()));
                }
            }

            // Step 3: Clone the updated document and validate against schema.
            let new_value = doc.clone();
            #[cfg(feature = "schema")]
            crate::engine::schema::validate_document(schemas, collection, &new_value)?;

            // Step 2: Update state as MsgPack bytes.
            if let Ok(bytes) = rmp_serde::to_vec(&new_value) {
                col.insert(key.to_string(), bytes.into_boxed_slice());
            }

            // Step 6: Write the full updated document as an INSERT entry.
            let entry = LogEntry::new(
                "INSERT".to_string(),
                collection.to_string(),
                key.to_string(),
                new_value.clone(),
            );
            storage.write_entry(&entry)?;

            // TX_COMMIT: Successfully complete the transaction.
            storage.write_entry(&LogEntry::new(
                "TX_COMMIT".into(),
                collection.into(),
                tx_id,
                Value::Null,
            ))?;

            // Step 7: Broadcast a lean change event to WebSocket subscribers.
            let new_v = new_value.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
            let _ = tx.send(
                json!({
                    "event": "change",
                    "collection": collection,
                    "key": key,
                    "new_v": new_v
                })
                .to_string(),
            );
            return Ok(true); // document was found and updated
        }

    // If document not found, we still commit the transaction (which was just a BEGIN).
    // Alternatively, we could have started the transaction only after finding the document.
    // Given the current architecture, starting it at the top is safer for consistency.
    storage.write_entry(&LogEntry::new(
        "TX_COMMIT".into(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

    Ok(false) // document not found — no-op
}
