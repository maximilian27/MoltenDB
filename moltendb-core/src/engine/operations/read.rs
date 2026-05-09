// ─── operations/read.rs ───────────────────────────────────────────────────────
// Read operations: get, get_all, get_filtered, scan_top_n.
// All documents are always in RAM — no Cold/disk reads needed.
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::DashMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use super::super::StorageBackend;

/// Retrieve a specific set of documents by their keys.
pub fn get(
    state: &DashMap<String, DashMap<String, Value>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
    keys: Vec<String>,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        for key in keys {
            if let Some(v) = col.get(&key) {
                results.insert(key, v.clone());
            }
        }
    }
    results
}

/// Retrieve documents matching a predicate, scanning lazily.
pub fn get_filtered(
    state: &DashMap<String, DashMap<String, Value>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
    predicate: impl Fn(&Value) -> bool,
    offset: usize,
    limit: Option<usize>,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    let mut skipped = 0usize;
    if let Some(col) = state.get(collection) {
        for entry in col.iter() {
            let v = entry.value();
            if !predicate(v) { continue; }
            if skipped < offset { skipped += 1; continue; }
            results.insert(entry.key().clone(), v.clone());
            if let Some(lim) = limit && results.len() >= lim {
                break;
            }
        }
    }
    results
}

/// Lazily scan a collection and return the top-N documents according to a
/// comparator, applying an optional predicate along the way.
pub fn scan_top_n<P, C>(
    state: &DashMap<String, DashMap<String, Value>>,
    _storage: &Arc<dyn StorageBackend>,
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
            let v = entry.value();
            if !predicate(v) { continue; }
            if heap.len() >= cap
                && let Some(worst) = heap.peek()
                    && cmp(v, &worst.value) != Ordering::Less {
                        continue;
                    }
            heap.push(HeapItem { key: entry.key().clone(), value: v.clone(), cmp: &cmp });
            if heap.len() > cap { heap.pop(); }
        }
    }

    heap.into_sorted_vec().into_iter().map(|h| (h.key, h.value)).collect()
}

/// Retrieve all documents in a collection as a HashMap.
pub fn get_all(
    state: &DashMap<String, DashMap<String, Value>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        for entry in col.iter() {
            results.insert(entry.key().clone(), entry.value().clone());
        }
    }
    results
}
