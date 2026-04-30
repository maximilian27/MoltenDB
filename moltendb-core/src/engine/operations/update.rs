// ─── operations/update.rs ─────────────────────────────────────────────────────
// Update operation: update (partial patch).
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::{DashMap, DashSet};
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::debug;
use super::common::now_iso;
use super::super::{indexing, StorageBackend};
use super::super::types::{DbError, LogEntry};

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
pub fn update(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    #[cfg(feature = "schema")] schemas: &DashMap<String, Arc<(Value, jsonschema::Validator)>>,
    collection: &str,
    key: &str,
    updates: Value, // the partial update — only these fields will be changed
) -> Result<bool, DbError> {
    // TX_BEGIN: Start a transaction for the update.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    if let Some(col) = state.get(collection) {
        if let Some(doc) = {
            if let Some(doc_state) = col.get(key) {
                // Fetch the full document value first.
                Some(match doc_state.value() {
                    crate::engine::types::DocumentState::Hot(v) => v.clone(),
                    crate::engine::types::DocumentState::Cold(ptr) => {
                        let bytes = storage.read_at(ptr.offset, ptr.length)?;
                        let entry: crate::engine::types::LogEntry = serde_json::from_slice(&bytes)?;
                        entry.value
                    }
                })
            } else {
                None
            }
        } {
            let mut doc = doc;

            // Step 1: Remove the document from indexes BEFORE modifying it,
            // so the old field values are removed from the index entries.
            indexing::unindex_doc(indexes, collection, key, &doc);

            // Step 2: Merge the update fields into the existing document.
            // Only top-level fields are merged — nested objects are replaced,
            // not recursively merged.
            if let Some(update_obj) = updates.as_object() {
                // If the caller provides a "_v" field in the update, it acts as a guard.
                // If the current version is not equal to this guard, we return Conflict.
                let existing_v = doc.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(guard_v) = update_obj.get("_v").and_then(|v| v.as_u64()) {
                    if guard_v != existing_v {
                        debug!("⚡ Conflict error: {}/{} update guard _v={} != stored _v={}", collection, key, guard_v, existing_v);
                        return Err(DbError::Conflict);
                    }
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

            // Step 4: Re-add the document to indexes with its new field values.
            indexing::index_doc(indexes, collection, key, &new_value);

            // Step 5: Update state (now Hot).
            col.insert(key.to_string(), crate::engine::types::DocumentState::Hot(new_value.clone()));

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
