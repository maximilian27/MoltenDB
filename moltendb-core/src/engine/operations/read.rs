// ─── operations/read.rs ───────────────────────────────────────────────────────
// Read operations: get, get_all.
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::DashMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use super::super::{StorageBackend};

/// Retrieve a specific set of documents by their keys.
///
/// Only returns documents that actually exist — missing keys are silently
/// skipped. Returns an empty HashMap if the collection doesn't exist.
/// Pass a single key to retrieve one document.
pub fn get(
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
                        if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length)
                            && let Ok(log_entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                                results.insert(key, log_entry.value);
                            }
                    }
                }
            }
        }
    }
    results
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
                    if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length)
                        && let Ok(log_entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                            results.insert(key.clone(), log_entry.value);
                        }
                }
            }
        }
    }
    results
}

