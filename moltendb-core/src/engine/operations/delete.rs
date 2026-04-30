// ─── operations/delete.rs ─────────────────────────────────────────────────────
// Delete operations: delete, delete_batch, delete_collection.
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::{DashMap, DashSet};
use serde_json::{json, Value};
use std::sync::Arc;
use super::super::{indexing, StorageBackend};
use super::super::types::{DbError, LogEntry};

/// Delete a single document from a collection.
///
/// If the document doesn't exist, this is a no-op (no error).
/// A DELETE LogEntry is always written to the log, even if the document
/// didn't exist in memory — this ensures the log is consistent.
pub fn delete(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    key: &str,
) -> Result<(), DbError> {
    // TX_BEGIN: Start a transaction for the delete.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    if let Some(col) = state.get(collection) {
        if let Some(val) = {
            if let Some(doc_state) = col.get(key) {
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
            indexing::unindex_doc(indexes, collection, key, &val);
        }
        // Remove the document from the in-memory collection.
        col.remove(key);
    }

    // Write a DELETE entry to the log.
    let entry = LogEntry::new(
        "DELETE".to_string(),
        collection.to_string(),
        key.to_string(),
        json!(null),
    );
    storage.write_entry(&entry)?;

    // TX_COMMIT: Successfully complete the transaction.
    storage.write_entry(&LogEntry::new(
        "TX_COMMIT".into(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

    // Broadcast a lean delete event to WebSocket subscribers.
    let _ = tx.send(
        json!({
            "event": "change",
            "collection": collection,
            "key": key,
            "new_v": null
        })
        .to_string(),
    );
    Ok(())
}

/// Delete multiple documents from a collection in a single call.
///
/// Each document is removed from indexes and state individually, and a
/// separate DELETE LogEntry is written for each key. If the collection
/// doesn't exist, this is a no-op.
pub fn delete_batch(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    keys: Vec<String>,
) -> Result<(), DbError> {
    // TX_BEGIN: Start a transaction for the batch delete.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    if let Some(col) = state.get(collection) {
        for key in keys {
            // Remove from indexes before removing from state.
            if let Some(val) = {
                if let Some(doc_state) = col.get(&key) {
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
                indexing::unindex_doc(indexes, collection, &key, &val);
            }

            // Remove the document from the in-memory collection.
            col.remove(&key);

            // Write a DELETE entry for this key.
            let entry = LogEntry::new(
                "DELETE".to_string(),
                collection.to_string(),
                key.clone(),
                json!(null),
            );
            storage.write_entry(&entry)?;

            // Broadcast a lean delete event.
            let event = json!({
                "event": "change",
                "collection": collection,
                "key": key,
                "new_v": null
            })
            .to_string();
            let _ = tx.send(event);
        }
    }

    // TX_COMMIT: Successfully complete the transaction.
    storage.write_entry(&LogEntry::new(
        "TX_COMMIT".into(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

    Ok(())
}

/// Drop an entire collection — removes all documents and its indexes.
///
/// This is an irreversible operation. A DROP LogEntry is written to the log
/// so the collection is not recreated on the next startup.
///
/// After this call:
///   - The collection no longer exists in the in-memory state.
///   - All indexes for this collection are removed.
///   - The DROP entry in the log ensures the collection stays gone on restart.
pub fn delete_collection(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
) -> Result<(), DbError> {
    // TX_BEGIN: Start a transaction for the drop.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    // Step 1: Remove from memory.
    state.remove(collection);
    // Step 2: Remove all indexes for this collection.
    indexes.retain(|k, _| !k.starts_with(&format!("{}:", collection)));

    // Step 3: Persist the DROP command.
    let entry = LogEntry::new(
        "DROP".to_string(),
        collection.to_string(),
        "*".to_string(),
        json!(null),
    );
    storage.write_entry(&entry)?;

    // TX_COMMIT: Successfully complete the transaction.
    storage.write_entry(&LogEntry::new(
        "TX_COMMIT".into(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

    // Step 4: Broadcast a lean drop event.
    let event = json!({
        "event": "change",
        "collection": collection,
        "key": "*",
        "new_v": null
    })
    .to_string();
    let _ = tx.send(event);
    Ok(())
}
