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
use crate::query::read_msgpack_seq;
#[cfg(not(target_arch = "wasm32"))]
use crate::query::evaluate_numeric_simd_batch;

#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

/// Decode a stored MsgPack byte slice to a serde_json::Value.
/// Returns None if deserialization fails (should never happen for well-formed data).
#[inline]
fn decode(bytes: &[u8]) -> Option<Value> {
    rmp_serde::from_slice(bytes).ok()
}

/// Retrieve a specific set of documents by their keys.
pub fn get(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
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
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
    predicate: impl Fn(&str, &[u8]) -> bool + Sync + Send,
    offset: usize,
    count: Option<usize>,
) -> Vec<(String, Value)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let col = match state.get(collection) {
            Some(c) => c,
            None => return Vec::new(),
        };

        // Phase 1: collect (seq, key) pairs for docs that pass the predicate.
        // We deliberately do NOT decode to Value here so that filtered-out documents
        // (the majority for $ne/$nin queries) never get deserialized.
        let mut matching: Vec<(u64, String)> = col
            .par_iter()
            .filter_map(|entry| {
                if predicate(entry.key(), entry.value()) {
                    let seq = read_msgpack_seq(entry.value());
                    Some((seq, entry.key().clone()))
                } else {
                    None
                }
            })
            .collect();

        // Phase 2: sort by insertion order (_seq), then slice to the requested page.
        matching.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let end = match count {
            Some(l) => (offset + l).min(matching.len()),
            None => matching.len(),
        };
        let start = offset.min(matching.len());
        let page = &matching[start..end];

        // Phase 3: decode only the documents on the requested page.
        page
            .iter()
            .filter_map(|(_, k)| {
                let v = decode(col.get(k)?.value())?;
                Some((k.clone(), v))
            })
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut results = Vec::new();
        let mut skipped = 0usize;
        if let Some(col) = state.get(collection) {
            for entry in col.iter() {
                if !predicate(entry.key(), entry.value()) { continue; }
                let v = match decode(entry.value()) {
                    Some(v) => v,
                    None => continue,
                };
                if skipped < offset { skipped += 1; continue; }
                results.push((entry.key().clone(), v));
                if let Some(lim) = count && results.len() >= lim {
                    break;
                }
            }
        }
        results
    }
}

/// Scan a collection filtering by a single numeric range predicate, using
/// SIMD (`f64x4`) to evaluate 4 documents per cycle on native targets.
///
/// This is the hot path for queries like `{ "price": { "$gt": 100 } }` over
/// large collections. The field value is extracted from raw MsgPack bytes at
/// the given dot-notation `field_path`; 4 docs are batched into a `f64x4` and
/// compared in one SIMD instruction. Docs where the field is missing or
/// non-numeric are excluded (treated as non-matching).
///
/// Falls back to scalar `evaluate_predicate_msgpack` for the tail (< 4 docs).
#[cfg(not(target_arch = "wasm32"))]
pub fn get_filtered_numeric_simd(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    collection: &str,
    field_path: &str,
    operator: &str,
    threshold: f64,
    offset: usize,
    count: Option<usize>,
) -> Vec<(String, Value)> {
    use rayon::prelude::*;

    let col = match state.get(collection) {
        Some(c) => c,
        None => return Vec::new(),
    };

    // Collect all (key, bytes-ref) pairs so we can chunk them for SIMD.
    // DashMap shards are already spread across threads via par_iter; we collect
    // keys first (cheap — just Arc clones) then process in SIMD chunks.
    let entries: Vec<(String, Box<[u8]>)> = col
        .par_iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();

    // Process in chunks of 4 using SIMD; handle the tail scalarly.
    let field_path = field_path.to_string();
    let operator = operator.to_string();

    let mut matching: Vec<(u64, String)> = entries
        .par_chunks(4)
        .flat_map_iter(|chunk| {
            let mut out = Vec::with_capacity(chunk.len());
            if chunk.len() == 4 {
                // Full SIMD batch — 4 docs in one f64x4 comparison.
                let docs = [
                    chunk[0].1.as_ref(),
                    chunk[1].1.as_ref(),
                    chunk[2].1.as_ref(),
                    chunk[3].1.as_ref(),
                ];
                if let Some(results) = evaluate_numeric_simd_batch(docs, &field_path, &operator, threshold) {
                    for (i, matched) in results.iter().enumerate() {
                        if *matched {
                            let seq = read_msgpack_seq(&chunk[i].1);
                            out.push((seq, chunk[i].0.clone()));
                        }
                    }
                }
            } else {
                // Tail: scalar fallback for remaining 1–3 docs.
                for (key, bytes) in chunk {
                    let matched = crate::query::evaluate_predicate_msgpack(
                        bytes, &field_path, &operator,
                        &serde_json::Value::Number(
                            serde_json::Number::from_f64(threshold).unwrap_or(serde_json::Number::from(0))
                        ),
                    ).unwrap_or(false);
                    if matched {
                        let seq = read_msgpack_seq(bytes);
                        out.push((seq, key.clone()));
                    }
                }
            }
            out
        })
        .collect();

    matching.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let end = match count {
        Some(l) => (offset + l).min(matching.len()),
        None => matching.len(),
    };
    let start = offset.min(matching.len());
    let page = &matching[start..end];

    page.iter()
        .filter_map(|(_, k)| {
            let v = decode(col.get(k)?.value())?;
            Some((k.clone(), v))
        })
        .collect()
}

/// Scan a collection and return the top-N documents according to a comparator,
/// applying an optional predicate along the way.
///
/// Native: each rayon worker maintains its own bounded heap, results are merged
/// at the end.
/// Wasm: single-threaded bounded heap.
pub fn scan_top_n<P, C>(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
    predicate: P,
    cmp: C,
    cap: usize,
) -> Vec<(String, Value)>
where
    P: Fn(&Value) -> bool + Sync,
    C: Fn(&Value, &Value) -> std::cmp::Ordering + Send + Sync,
{
    use std::collections::BinaryHeap;
    use std::cmp::Ordering;
    if cap == 0 {
        return Vec::new();
    }
    struct HeapItem<F: Fn(&Value, &Value) -> Ordering> {
        key: String,
        value: Value,
        cmp: Arc<F>,
    }
    impl<F: Fn(&Value, &Value) -> Ordering> PartialEq for HeapItem<F> {
        fn eq(&self, o: &Self) -> bool {
            (self.cmp)(&self.value, &o.value) == Ordering::Equal && self.key == o.key
        }
    }
    impl<F: Fn(&Value, &Value) -> Ordering> Eq for HeapItem<F> {}
    impl<F: Fn(&Value, &Value) -> Ordering> PartialOrd for HeapItem<F> {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> { Some(self.cmp(o)) }
    }
    impl<F: Fn(&Value, &Value) -> Ordering> Ord for HeapItem<F> {
        fn cmp(&self, o: &Self) -> Ordering {
            // Break ties by key so the heap is deterministic across pages.
            (self.cmp)(&self.value, &o.value).then_with(|| self.key.cmp(&o.key))
        }
    }

    let cmp = Arc::new(cmp);

    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(col) = state.get(collection) else { return Vec::new(); };
        // Each rayon worker keeps its own bounded heap (size ≤ cap) and we merge
        // them at the end. Peak memory is O(workers * cap) instead of
        // O(collection size), and we avoid materialising a giant intermediate Vec
        // — which is the dominant cost for sort-only queries over 1M docs.
        let push_into = |heap: &mut BinaryHeap<HeapItem<C>>, k: String, v: Value, cmp: &Arc<C>| {
            let candidate = HeapItem { key: k, value: v, cmp: cmp.clone() };
            if heap.len() >= cap {
                // Only evict the current worst if the candidate is strictly better.
                if let Some(worst) = heap.peek() {
                    if candidate.cmp(worst) != Ordering::Less {
                        return;
                    }
                }
            }
            heap.push(candidate);
            if heap.len() > cap { heap.pop(); }
        };

        let merged: BinaryHeap<HeapItem<C>> = col
            .par_iter()
            .fold(
                || BinaryHeap::<HeapItem<C>>::with_capacity(cap + 1),
                |mut heap, entry| {
                    if let Some(v) = decode(entry.value())
                        && predicate(&v)
                    {
                        push_into(&mut heap, entry.key().clone(), v, &cmp);
                    }
                    heap
                },
            )
            .reduce(
                || BinaryHeap::<HeapItem<C>>::with_capacity(cap + 1),
                |mut a, b| {
                    for item in b {
                        push_into(&mut a, item.key, item.value, &cmp);
                    }
                    a
                },
            );
        merged.into_sorted_vec().into_iter().map(|h| (h.key, h.value)).collect()
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
                let candidate = HeapItem { key: entry.key().clone(), value: v, cmp: cmp.clone() };
                if heap.len() >= cap {
                    if let Some(worst) = heap.peek() {
                        if candidate.cmp(worst) != Ordering::Less { continue; }
                    }
                }
                heap.push(candidate);
                if heap.len() > cap { heap.pop(); }
            }
        }
        heap.into_sorted_vec().into_iter().map(|h| (h.key, h.value)).collect()
    }
}

/// Retrieve all documents in a collection.
pub fn get_all(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    _storage: &Arc<dyn StorageBackend>,
    collection: &str,
    offset: usize,
    count: Option<usize>,
) -> Vec<(String, Value)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let col = match state.get(collection) {
            Some(c) => c,
            None => return Vec::new(),
        };
        // Collect (seq, key) so we can sort by insertion order before paging.
        let mut pairs: Vec<(u64, String)> = col
            .par_iter()
            .map(|entry| (read_msgpack_seq(entry.value()), entry.key().clone()))
            .collect();
        pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let end = match count {
            Some(l) => (offset + l).min(pairs.len()),
            None => pairs.len(),
        };
        let start = offset.min(pairs.len());
        pairs[start..end]
            .iter()
            .filter_map(|(_, k)| decode(col.get(k)?.value()).map(|v| (k.clone(), v)))
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut results = Vec::new();
        let mut skipped = 0usize;
        if let Some(col) = state.get(collection) {
            for entry in col.iter() {
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if let Some(val) = decode(entry.value()) {
                    results.push((entry.key().clone(), val));
                    if let Some(lim) = count && results.len() >= lim {
                        break;
                    }
                }
            }
        }
        results
    }
}
