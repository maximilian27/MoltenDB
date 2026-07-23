// ─── operations/delete.rs ─────────────────────────────────────────────────────
// Delete operations: delete, delete_filtered, delete_collection.
// ─────────────────────────────────────────────────────────────────────────────

use super::super::types::{DbError, LogEntry};
use super::super::StorageBackend;
use crate::common::system_fields::SystemFields;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// Delete one or more documents from a collection in a single call.
///
/// Each document is removed from indexes and state individually, and a
/// separate DELETE LogEntry is written for each key. If the collection
/// doesn't exist, this is a no-op. Pass a single key to delete one document.
pub fn delete(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    seq_index: &DashMap<Arc<str>, Arc<RwLock<BTreeMap<u64, String>>>>,
    collection: &str,
    keys: Vec<String>,
) -> Result<(), DbError> {
    // TX_BEGIN: Start a transaction for the batch delete.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        crate::common::log_commands::LogCommand::IKEY_TX_BEGIN.to_string(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    if let Some(col) = state.get(collection) {
        for key in keys {
            // Remove from seq_index before removing from col (need the seq).
            if let Some(entry) = col.get(&key) {
                let seq = crate::common::system_field_tokens::read_msgpack_seq_token(&entry);
                if let Some(idx) = seq_index.get(collection) {
                    if let Ok(mut map) = idx.write() {
                        map.remove(&seq);
                    }
                }
            }
            // Remove the document from the in-memory collection.
            col.remove(&key);

            // Write a DELETE entry for this key.
            let entry = LogEntry::new(
                crate::common::log_commands::LogCommand::IKEY_DELETE.to_string(),
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
        crate::common::log_commands::LogCommand::IKEY_TX_COMMIT.to_string(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

    Ok(())
}

/// Scan a collection with a predicate and delete all matching documents.
///
/// Mirrors `get_filtered` on the read side — the predicate runs on the raw
/// MsgPack bytes (no full `serde_json::Value` decode per document), so a bulk
/// delete pays the same cheap scan cost as an unsorted GET. Matches are
/// collected as `(seq, key)` pairs and ordered by `_seq` before the
/// `count_limit` is applied, so a limited delete is deterministic:
///   - `default_order_asc == true`  → oldest documents first (lowest `_seq`),
///   - `default_order_asc == false` → newest documents first (highest `_seq`).
/// If `count_limit` is `Some(n)`, at most `n` documents are deleted.
/// Returns the number of documents deleted.
pub fn delete_filtered(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    seq_index: &DashMap<Arc<str>, Arc<RwLock<BTreeMap<u64, String>>>>,
    collection: &str,
    predicate: impl Fn(&str, &[u8]) -> bool + Sync,
    count_limit: Option<usize>,
    default_order_asc: bool,
) -> Result<usize, DbError> {
    use crate::common::system_field_tokens::read_msgpack_seq_token;

    // Phase 1: collect (seq, key) pairs for matches only — predicate runs on
    // raw bytes and we read just the cheap `_seq` token, no full decode.
    let mut pairs: Vec<(u64, String)> = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            match state.get(collection) {
                Some(col) => col
                    .par_iter()
                    .filter_map(|entry| {
                        if predicate(entry.key(), entry.value()) {
                            Some((read_msgpack_seq_token(entry.value()), entry.key().clone()))
                        } else {
                            None
                        }
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
                        if predicate(entry.key(), entry.value()) {
                            Some((read_msgpack_seq_token(entry.value()), entry.key().clone()))
                        } else {
                            None
                        }
                    })
                    .collect(),
                None => return Ok(0),
            }
        }
    };

    // Order by _seq so a count-limited delete is deterministic and consistent
    // with GET: ascending = oldest first (default), descending = newest first.
    if default_order_asc {
        pairs.sort_unstable_by_key(|(seq, _)| *seq);
    } else {
        pairs.sort_unstable_by_key(|(seq, _)| std::cmp::Reverse(*seq));
    }

    let mut keys: Vec<String> = pairs.into_iter().map(|(_, k)| k).collect();
    if let Some(limit) = count_limit {
        keys.truncate(limit);
    }

    let count = keys.len();
    if count == 0 {
        return Ok(0);
    }
    delete(state, storage, tx, seq_index, collection, keys)?;
    Ok(count)
}

/// Delete the `n` oldest or newest documents from a collection by `_seq`.
///
/// This is the count-only sibling of `delete_filtered` — no predicate is
/// evaluated. It reuses the ordered `seq_index` `BTreeMap` (the same structure
/// the unsorted GET path uses) to pick the first/last `n` keys directly, so it
/// never scans, decodes, or reads the `_seq` token per document (the `_seq` is
/// the `BTreeMap` key):
///   - `order_asc == true`  → oldest documents first (lowest `_seq`),
///   - `order_asc == false` → newest documents first (highest `_seq`).
/// If the collection has fewer than `n` documents, all of them are removed. If
/// the `seq_index` has not been built yet, it falls back to a scan + sort by
/// `_seq` (mirroring `get_filtered`'s fallback). Returns the number deleted.
pub fn delete_n(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    seq_index: &DashMap<Arc<str>, Arc<RwLock<BTreeMap<u64, String>>>>,
    collection: &str,
    n: usize,
    order_asc: bool,
) -> Result<usize, DbError> {
    use crate::common::system_field_tokens::read_msgpack_seq_token;

    if n == 0 {
        return Ok(0);
    }

    let col = match state.get(collection) {
        Some(c) => c,
        None => return Ok(0),
    };

    // Prefer the ordered `seq_index`: take the first/last `n` keys straight from
    // the `BTreeMap` — no scan, no decode, no `_seq` token read.
    let keys: Vec<String> = if let Some(idx) = seq_index.get(collection) {
        match idx.read() {
            Ok(map) => {
                if order_asc {
                    map.iter().take(n).map(|(_, k)| k.clone()).collect()
                } else {
                    map.iter().rev().take(n).map(|(_, k)| k.clone()).collect()
                }
            }
            Err(_) => Vec::new(),
        }
    } else {
        // Fallback: seq_index not yet built — scan and sort by `_seq`.
        let mut pairs: Vec<(u64, String)> = col
            .iter()
            .map(|e| (read_msgpack_seq_token(e.value()), e.key().clone()))
            .collect();
        if order_asc {
            pairs.sort_unstable_by_key(|(seq, _)| *seq);
        } else {
            pairs.sort_unstable_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        }
        pairs.into_iter().take(n).map(|(_, k)| k).collect()
    };

    let count = keys.len();
    if count == 0 {
        return Ok(0);
    }
    drop(col); // release the read guard before calling delete
    delete(state, storage, tx, seq_index, collection, keys)?;
    Ok(count)
}

/// Evict the `n` oldest documents from a collection by `_seq` (lowest values first).
///
/// Used by the `maxSize` cap — after an insert batch pushes the collection over
/// its limit, this removes exactly `n` documents to bring it back to `maxSize`.
/// If the collection has fewer than `n` documents, all are removed.
/// Errors are silently ignored (best-effort eviction).
pub fn evict_oldest(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    seq_index: &DashMap<Arc<str>, Arc<RwLock<BTreeMap<u64, String>>>>,
    collection: &str,
    n: usize,
) {
    #[inline]
    fn decode(bytes: &[u8]) -> Option<Value> {
        crate::common::system_field_tokens::msgpack_to_value(bytes)
    }

    let col = match state.get(collection) {
        Some(c) => c,
        None => return,
    };

    // Collect (seq, key) pairs, then sort ascending by seq to find the oldest.
    let mut entries: Vec<(u64, String)> = col
        .iter()
        .filter_map(|e| {
            let v = decode(e.value())?;
            let seq = v
                .get(SystemFields::IKEY_SEQ)
                .and_then(|s| s.as_u64())
                .unwrap_or(u64::MAX);
            Some((seq, e.key().clone()))
        })
        .collect();

    entries.sort_unstable_by_key(|(seq, _)| *seq);
    entries.truncate(n);

    let keys: Vec<String> = entries.into_iter().map(|(_, k)| k).collect();
    if keys.is_empty() {
        return;
    }
    drop(col); // release the read guard before calling delete
    let _ = delete(state, storage, tx, seq_index, collection, keys);
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
    seq_index: &DashMap<Arc<str>, Arc<RwLock<BTreeMap<u64, String>>>>,
    collection: &str,
) -> Result<(), DbError> {
    // TX_BEGIN: Start a transaction for the drop.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        crate::common::log_commands::LogCommand::IKEY_TX_BEGIN.to_string(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    // Step 1: Remove from memory and seq_index.
    state.remove(collection);
    seq_index.remove(collection);
    // Step 2: Persist the DROP command.
    let entry = LogEntry::new(
        crate::common::log_commands::LogCommand::IKEY_DROP.to_string(),
        collection.to_string(),
        "*".to_string(),
        json!(null),
    );
    storage.write_entry(&entry)?;

    // TX_COMMIT: Successfully complete the transaction.
    storage.write_entry(&LogEntry::new(
        crate::common::log_commands::LogCommand::IKEY_TX_COMMIT.to_string(),
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
