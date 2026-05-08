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

/// Lazily scan a collection and return the top-N documents according to a
/// comparator, applying an optional predicate (e.g. a WHERE clause) along
/// the way.
///
/// Documents flow directly from the DashMap into a bounded max-heap of
/// capacity `cap` (= `offset + count`). When the heap is full, the *worst*
/// candidate is evicted on every push, so peak memory is `O(cap)` extra
/// allocations on top of the (unavoidable) DashMap iteration — even for
/// collections of millions of documents this stays in the hundreds of KB.
///
/// Returns documents in best-first order (already sorted), capped at `cap`.
/// The caller still needs to apply `offset` (skip first N) and `count` (take).
///
/// `cmp` returns `Ordering::Less` for "better" items (the user comparator
/// pattern used elsewhere in process_get); under a max-heap that means the
/// *worst* item bubbles to the top and is evicted first. `into_sorted_vec`
/// then yields best-first.
pub fn scan_top_n<P, C>(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &Arc<dyn StorageBackend>,
    collection: &str,
    predicate: P,
    cmp: C,
    cap: usize,
) -> Vec<(String, Value)>
where
    P: Fn(&Value) -> bool,
    C: Fn(&Value, &Value) -> std::cmp::Ordering,
{
    use std::collections::BinaryHeap;
    use std::cmp::Ordering;

    if cap == 0 {
        return Vec::new();
    }

    // HeapItem wraps (key, value) and uses the user comparator directly so
    // the worst item has the greatest ordering — exactly what a max-heap
    // needs to evict the worst on overflow.
    struct HeapItem<'a, F: Fn(&Value, &Value) -> Ordering> {
        key: String,
        value: Value,
        cmp: &'a F,
    }
    impl<'a, F: Fn(&Value, &Value) -> Ordering> PartialEq for HeapItem<'a, F> {
        fn eq(&self, o: &Self) -> bool { (self.cmp)(&self.value, &o.value) == Ordering::Equal }
    }
    impl<'a, F: Fn(&Value, &Value) -> Ordering> Eq for HeapItem<'a, F> {}
    impl<'a, F: Fn(&Value, &Value) -> Ordering> PartialOrd for HeapItem<'a, F> {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> { Some(self.cmp(o)) }
    }
    impl<'a, F: Fn(&Value, &Value) -> Ordering> Ord for HeapItem<'a, F> {
        fn cmp(&self, o: &Self) -> Ordering { (self.cmp)(&self.value, &o.value) }
    }

    let mut heap: BinaryHeap<HeapItem<C>> = BinaryHeap::with_capacity(cap + 1);

    if let Some(col) = state.get(collection) {
        for entry in col.iter() {
            // Extract the document value lazily. For Hot we can borrow first
            // and avoid cloning if predicate fails or the heap is full and
            // the candidate would be evicted immediately.
            match entry.value() {
                crate::engine::types::DocumentState::Hot(v) => {
                    if !predicate(v) { continue; }
                    // Quick "can't beat the worst" check before cloning when
                    // the heap is already at capacity.
                    if heap.len() >= cap
                        && let Some(worst) = heap.peek()
                            && cmp(v, &worst.value) != Ordering::Less {
                                // v is not strictly better than the current worst
                                // — it would be evicted immediately, so skip the clone.
                                continue;
                            }
                    heap.push(HeapItem { key: entry.key().clone(), value: v.clone(), cmp: &cmp });
                    if heap.len() > cap { heap.pop(); }
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
                    if heap.len() >= cap
                        && let Some(worst) = heap.peek()
                            && cmp(&log_entry.value, &worst.value) != Ordering::Less {
                                continue;
                            }
                    heap.push(HeapItem { key: entry.key().clone(), value: log_entry.value, cmp: &cmp });
                    if heap.len() > cap { heap.pop(); }
                }
            }
        }
    }

    // Drain in sorted order (best-first thanks to our reversed cmp).
    heap.into_sorted_vec().into_iter().map(|h| (h.key, h.value)).collect()
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

