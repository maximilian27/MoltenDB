// ─── operations/read.rs ───────────────────────────────────────────────────────
// Read operations: get, get_all, get_batch.
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::DashMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use super::super::{StorageBackend};

/// Retrieve a single document by its key from a collection.
///
/// Returns `Some(value)` if the document exists, `None` if the collection or
/// key doesn't exist. This is an O(1) hash map lookup.
///
/// In the hybrid Bitcask model, if the document is "Cold", it is fetched from
/// storage, deserialized, and returned.
pub fn get(
    // The full in-memory state: collection name → (key → document state).
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &Arc<dyn StorageBackend>,
    collection: &str,
    key: &str,
) -> Option<Value> {
    let col = state.get(collection)?;
    let doc_state = col.get(key)?;
    
    match doc_state.value() {
        crate::engine::types::DocumentState::Hot(v) => Some(v.clone()),
        crate::engine::types::DocumentState::Cold(ptr) => {
            // Fetch from disk.
            if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length) {
                // The bytes in the log are a full LogEntry JSON.
                if let Ok(entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                    return Some(entry.value);
                }
            }
            None
        }
    }
}

/// Retrieve all documents in a collection as a HashMap.
///
/// Returns an empty HashMap if the collection doesn't exist.
/// This is O(n) in the number of documents — it copies every document.
pub fn get_all(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &Arc<dyn StorageBackend>,
    collection: &str,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        for entry in col.iter() {
            let key = entry.key();
            match entry.value() {
                crate::engine::types::DocumentState::Hot(v) => {
                    results.insert(key.clone(), v.clone());
                }
                crate::engine::types::DocumentState::Cold(ptr) => {
                    if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length) {
                        if let Ok(log_entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                            results.insert(key.clone(), log_entry.value);
                        }
                    }
                }
            }
        }
    }
    results
}

/// Retrieve a specific set of documents by their keys (batch get).
///
/// Only returns documents that actually exist — missing keys are silently
/// skipped. Returns an empty HashMap if the collection doesn't exist.
pub fn get_batch(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &Arc<dyn StorageBackend>,
    collection: &str,
    keys: Vec<String>,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        for key in keys {
            if let Some(entry) = col.get(&key) {
                match entry.value() {
                    crate::engine::types::DocumentState::Hot(v) => {
                        results.insert(key, v.clone());
                    }
                    crate::engine::types::DocumentState::Cold(ptr) => {
                        if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length) {
                            if let Ok(log_entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                                results.insert(key, log_entry.value);
                            }
                        }
                    }
                }
            }
        }
    }
    results
}
