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

/// Retrieve documents matching a predicate, scanning lazily.
///
/// Iterates the collection without cloning every document up-front. Only
/// documents that pass `predicate` are cloned into the result map. This
/// dramatically reduces peak memory for sparse WHERE queries on large
/// collections (vs. `get_all` which materialises every document first).
///
/// `offset` and `limit` are applied during iteration so the scan can stop
/// early once enough matches have been collected. Pass `limit = None` to
/// scan the whole collection.
///
/// Cold (on-disk) documents are read from storage transparently; if a read
/// fails, the document is skipped.
pub fn get_filtered(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &Arc<dyn StorageBackend>,
    collection: &str,
    predicate: impl Fn(&Value) -> bool,
    offset: usize,
    limit: Option<usize>,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    let mut skipped = 0usize;
    if let Some(col) = state.get(collection) {
        for entry in col.iter() {
            // Materialise the document value (Hot = clone-on-match,
            // Cold = read-from-disk on-demand).
            let value: Value = match entry.value() {
                crate::engine::types::DocumentState::Hot(v) => {
                    if !predicate(v) { continue; }
                    v.clone()
                }
                crate::engine::types::DocumentState::Cold(ptr) => {
                    let bytes = match storage.read_at(ptr.offset, ptr.length) {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    let log_entry: crate::engine::types::LogEntry =
                        match serde_json::from_slice(&bytes) {
                            Ok(le) => le,
                            Err(_) => continue,
                        };
                    if !predicate(&log_entry.value) { continue; }
                    log_entry.value
                }
            };
            if skipped < offset { skipped += 1; continue; }
            results.insert(entry.key().clone(), value);
            if let Some(lim) = limit
                && results.len() >= lim {
                    break;
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

