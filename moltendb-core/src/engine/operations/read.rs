// ─── operations/read.rs ───────────────────────────────────────────────────────
// Read operations: get, get_all, get_filtered, scan_top_n.
// All documents are always in RAM — no Cold/disk reads needed.
// Documents are stored as MsgPack bytes (Box<[u8]>) and decoded to Value on read.
//
// On native targets the bulk-scan paths (`get_filtered`, `get_all`, `scan_top_n`)
// run in parallel via rayon — decoding MsgPack to `serde_json::Value` is the
// dominant cost for million-doc collections, so spreading it across CPU cores
// gives a near-linear speedup. On wasm32 we fall back to a sequential scan.
// ─────────────────────────────────────────────────────────────────────────────
use dashmap::DashMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use super::super::StorageBackend;

#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

/// Decode a MsgPack byte slice to a serde_json::Value.
/// Returns None if deserialization fails (should never happen for well-formed data).
#[inline]
fn decode(bytes: &[u8]) -> Option<Value> {
    rmp_serde::from_slice(bytes).ok()
}

/// Retrieve a specific set of documents by their keys.
pub fn get(
    state: &DashMap<String, DashMap<String, Box<[u8]>>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
    keys: Vec<String>,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        for key in keys {
            if let Some(v) = col.get(&key) {
                if let Some(val) = decode(&v) {
                    results.insert(key, val);
                }
            }
        }
    }
    results
}

/// Retrieve documents matching a predicate.
///
/// Native: scans the collection in parallel via rayon, then applies
/// `offset`/`limit` after collecting matches (order is non-deterministic, same
/// as the previous DashMap iteration order).
/// Wasm: sequential scan with early-stop on `limit`.
pub fn get_filtered(
    state: &DashMap<String, DashMap<String, Box<[u8]>>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
    predicate: impl Fn(&Value) -> bool + Sync,
    offset: usize,
    limit: Option<usize>,
) -> HashMap<String, Value> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut matches: Vec<(String, Value)> = match state.get(collection) {
            Some(col) => col
                .par_iter()
                .filter_map(|entry| {
                    let v = decode(entry.value())?;
                    if predicate(&v) {
                        Some((entry.key().clone(), v))
                    } else {
                        None
                    }
                })
                .collect(),
            None => return HashMap::new(),
        };
        // Apply offset / limit deterministically by key to keep responses stable
        // across runs even though parallel collection is unordered.
        if offset > 0 || limit.is_some() {
            matches.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        }
        let end = match limit {
            Some(l) => (offset + l).min(matches.len()),
            None => matches.len(),
        };
        let start = offset.min(matches.len());
        matches.drain(start..end).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut results = HashMap::new();
        let mut skipped = 0usize;
        if let Some(col) = state.get(collection) {
            for entry in col.iter() {
                let v = match decode(entry.value()) {
                    Some(v) => v,
                    None => continue,
                };
                if !predicate(&v) { continue; }
                if skipped < offset { skipped += 1; continue; }
                results.insert(entry.key().clone(), v);
                if let Some(lim) = limit && results.len() >= lim {
                    break;
                }
            }
        }
        results
    }
}

/// Scan a collection and return the top-N documents according to a comparator,
/// applying an optional predicate along the way.
///
/// Native: each rayon worker maintains its own bounded heap, results are merged
/// at the end.
/// Wasm: single-threaded bounded heap.
pub fn scan_top_n<P, C>(
    state: &DashMap<String, DashMap<String, Box<[u8]>>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
    predicate: P,
    cmp: C,
    cap: usize,
) -> Vec<(String, Value)>
where
    P: Fn(&Value) -> bool + Sync,
    C: Fn(&Value, &Value) -> std::cmp::Ordering + Sync,
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

    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(col) = state.get(collection) else { return Vec::new(); };
        // Parallel pre-filter: each worker decodes & predicates its DashMap shard
        // independently. The bounded top-N selection happens once on the merged
        // candidate set; it's O(matching) but the heavy MsgPack decode is what
        // we actually want parallelised.
        let candidates: Vec<(String, Value)> = col
            .par_iter()
            .filter_map(|entry| {
                let v = decode(entry.value())?;
                if predicate(&v) {
                    Some((entry.key().clone(), v))
                } else {
                    None
                }
            })
            .collect();

        let mut heap: BinaryHeap<HeapItem<C>> = BinaryHeap::with_capacity(cap + 1);
        for (k, v) in candidates {
            if heap.len() >= cap
                && let Some(worst) = heap.peek()
                && cmp(&v, &worst.value) != Ordering::Less
            {
                continue;
            }
            heap.push(HeapItem { key: k, value: v, cmp: &cmp });
            if heap.len() > cap { heap.pop(); }
        }
        heap.into_sorted_vec().into_iter().map(|h| (h.key, h.value)).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut heap: BinaryHeap<HeapItem<C>> = BinaryHeap::with_capacity(cap + 1);
        if let Some(col) = state.get(collection) {
            for entry in col.iter() {
                let v = match decode(entry.value()) {
                    Some(v) => v,
                    None => continue,
                };
                if !predicate(&v) { continue; }
                if heap.len() >= cap
                    && let Some(worst) = heap.peek()
                    && cmp(&v, &worst.value) != Ordering::Less
                {
                    continue;
                }
                heap.push(HeapItem { key: entry.key().clone(), value: v, cmp: &cmp });
                if heap.len() > cap { heap.pop(); }
            }
        }
        heap.into_sorted_vec().into_iter().map(|h| (h.key, h.value)).collect()
    }
}

/// Retrieve all documents in a collection as a HashMap.
pub fn get_all(
    state: &DashMap<String, DashMap<String, Box<[u8]>>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
) -> HashMap<String, Value> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        match state.get(collection) {
            Some(col) => col
                .par_iter()
                .filter_map(|entry| {
                    decode(entry.value()).map(|v| (entry.key().clone(), v))
                })
                .collect(),
            None => HashMap::new(),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut results = HashMap::new();
        if let Some(col) = state.get(collection) {
            for entry in col.iter() {
                if let Some(val) = decode(entry.value()) {
                    results.insert(entry.key().clone(), val);
                }
            }
        }
        results
    }
}
