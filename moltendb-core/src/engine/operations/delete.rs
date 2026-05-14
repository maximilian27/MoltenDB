// ─── operations/delete.rs ─────────────────────────────────────────────────────
// Delete operations: delete, delete_filtered, delete_collection.
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::Arc;
use super::super::StorageBackend;
use super::super::types::{DbError, LogEntry};


/// Delete one or more documents from a collection in a single call.
///
/// Each document is removed from indexes and state individually, and a
/// separate DELETE LogEntry is written for each key. If the collection
/// doesn't exist, this is a no-op. Pass a single key to delete one document.
pub fn delete(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
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

/// Scan a collection with a predicate and delete all matching documents.
///
/// Mirrors `get_filtered` on the read side — uses a parallel scan on native
/// targets to collect matching keys, then deletes them in a single transaction.
/// If `count_limit` is `Some(n)`, at most `n` documents are deleted.
/// Returns the number of documents deleted.
pub fn delete_filtered(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    predicate: impl Fn(&Value) -> bool + Sync,
    count_limit: Option<usize>,
) -> Result<usize, DbError> {
    #[inline]
    fn decode(bytes: &[u8]) -> Option<Value> {
        rmp_serde::from_slice(bytes).ok()
    }

    let keys: Vec<String> = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            match state.get(collection) {
                Some(col) => col
                    .par_iter()
                    .filter_map(|entry| {
                        let v = decode(entry.value())?;
                        if predicate(&v) { Some(entry.key().clone()) } else { None }
                    })
                    .collect(),
                None => return Ok(0),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            match state.get(collection) {
                Some(col) => col
                    .iter()
                    .filter_map(|entry| {
                        let v = decode(entry.value())?;
                        if predicate(&v) { Some(entry.key().clone()) } else { None }
                    })
                    .collect(),
                None => return Ok(0),
            }
        }
    };

    let mut keys = keys;
    if let Some(limit) = count_limit {
        keys.truncate(limit);
    }

    let count = keys.len();
    if count == 0 {
        return Ok(0);
    }
    delete(state, storage, tx, collection, keys)?;
    Ok(count)
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
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
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
    // Step 2: Persist the DROP command.
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
