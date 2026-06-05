// ─── operations/get ───────────────────────────────────────────────────────
// Read operations: get, get_all, get_filtered, scan_top_n.
// All documents are always in RAM — no Cold/disk reads needed.
// Documents are stored as MsgPack bytes (Box<[u8]>) and decoded to Value on read.
//
// On native targets the bulk-scan paths (`get_filtered`, `get_all`, `scan_top_n`)
// run in parallel via rayon — decoding MsgPack to `serde_json::Value` is the
// dominant cost for million-doc collections, so spreading it across CPU cores
// gives a near-linear speedup. On wasm32 we fall back to a sequential scan.
// ─────────────────────────────────────────────────────────────────────────────
use super::super::StorageBackend;
use crate::query::read_msgpack_seq;
use dashmap::DashMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

/// Decode a stored MsgPack byte slice to a serde_json::Value.
/// Returns None if deserialization fails (should never happen for well-formed data).
#[inline]
fn decode(bytes: &[u8]) -> Option<Value> {
    rmp_serde::from_slice(bytes).ok()
}

#[derive(Copy, Clone, Debug)]
pub struct CompactItem {
    pub sort_value: f64, // Extracted via raw MsgPack byte scan
    pub seq: u64,        // The monotonic document sequence number
}

impl PartialEq for CompactItem {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.seq == other.seq && self.sort_value == other.sort_value
    }
}

impl Eq for CompactItem {}

impl PartialOrd for CompactItem {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CompactItem {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.sort_value.total_cmp(&other.sort_value) {
            std::cmp::Ordering::Equal => self.seq.cmp(&other.seq),
            other_ord => other_ord,
        }
    }
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

        let limit = count.unwrap_or(usize::MAX);
        let deep = count.is_none() || (offset + limit > 5000);

        let page_items: Vec<CompactItem> = if !deep {
            use std::collections::BinaryHeap;
            let heap = col
                .par_iter()
                .fold(
                    || BinaryHeap::<CompactItem>::with_capacity(offset + limit + 1),
                    |mut h, entry| {
                        if predicate(entry.key(), entry.value()) {
                            let seq = read_msgpack_seq(entry.value());
                            let item = CompactItem {
                                sort_value: seq as f64,
                                seq,
                            };
                            if h.len() >= offset + limit {
                                if let Some(worst) = h.peek() {
                                    if item < *worst {
                                        h.pop();
                                        h.push(item);
                                    }
                                }
                            } else {
                                h.push(item);
                            }
                        }
                        h
                    },
                )
                .reduce(
                    || BinaryHeap::<CompactItem>::with_capacity(offset + limit + 1),
                    |mut a, b| {
                        for item in b {
                            if a.len() >= offset + limit {
                                if let Some(worst) = a.peek() {
                                    if item < *worst {
                                        a.pop();
                                        a.push(item);
                                    }
                                }
                            } else {
                                a.push(item);
                            }
                        }
                        a
                    },
                );
            let sorted = heap.into_sorted_vec();
            let start = offset.min(sorted.len());
            sorted[start..].to_vec()
        } else {
            let mut matching: Vec<CompactItem> = col
                .par_iter()
                .filter_map(|entry| {
                    if predicate(entry.key(), entry.value()) {
                        let seq = read_msgpack_seq(entry.value());
                        Some(CompactItem {
                            sort_value: seq as f64,
                            seq,
                        })
                    } else {
                        None
                    }
                })
                .collect();

            let total_len = matching.len();
            let start = offset;
            let end = match count {
                Some(l) => (offset + l).min(total_len),
                None => total_len,
            };

            if start < total_len {
                matching.select_nth_unstable_by(start, |a, b| a.cmp(b));
                if end < total_len {
                    matching[start..].select_nth_unstable_by(end - start, |a, b| a.cmp(b));
                }
                let page_slice = &mut matching[start..end];
                page_slice.sort_unstable_by(|a, b| a.cmp(b));
                page_slice.to_vec()
            } else {
                Vec::new()
            }
        };

        // Phase 2.5: hydration
        if page_items.is_empty() {
            return Vec::new();
        }

        use std::collections::HashSet;
        let seq_set: HashSet<u64> = page_items.iter().map(|item| item.seq).collect();
        use std::collections::HashMap;
        let hydrated: HashMap<u64, String> = col
            .par_iter()
            .filter_map(|entry| {
                let seq = read_msgpack_seq(entry.value());
                if seq_set.contains(&seq) {
                    Some((seq, entry.key().clone()))
                } else {
                    None
                }
            })
            .collect();

        // Phase 3: decode
        page_items
            .par_iter()
            .filter_map(|item| {
                let key = hydrated.get(&item.seq)?.clone();
                let entry = col.get(&key)?;
                let val = decode(entry.value())?;
                Some((key, val))
            })
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let col = match state.get(collection) {
            Some(c) => c,
            None => return Vec::new(),
        };

        let limit = count.unwrap_or(usize::MAX);
        let deep = count.is_none() || (offset + limit > 5000);

        let page_items: Vec<CompactItem> = if !deep {
            let mut heap = std::collections::BinaryHeap::with_capacity(offset + limit + 1);
            for entry in col.iter() {
                if predicate(entry.key(), entry.value()) {
                    let seq = read_msgpack_seq(entry.value());
                    let item = CompactItem {
                        sort_value: seq as f64,
                        seq,
                    };
                    if heap.len() >= offset + limit {
                        if let Some(worst) = heap.peek() {
                            if item < *worst {
                                heap.pop();
                                heap.push(item);
                            }
                        }
                    } else {
                        heap.push(item);
                    }
                }
            }
            let sorted = heap.into_sorted_vec();
            let start = offset.min(sorted.len());
            sorted[start..].to_vec()
        } else {
            let mut matching: Vec<CompactItem> = col
                .iter()
                .filter_map(|entry| {
                    if predicate(entry.key(), entry.value()) {
                        let seq = read_msgpack_seq(entry.value());
                        Some(CompactItem {
                            sort_value: seq as f64,
                            seq,
                        })
                    } else {
                        None
                    }
                })
                .collect();

            let total_len = matching.len();
            let start = offset;
            let end = match count {
                Some(l) => (offset + l).min(total_len),
                None => total_len,
            };

            if start < total_len {
                matching.select_nth_unstable_by(start, |a, b| a.cmp(b));
                if end < total_len {
                    matching[start..].select_nth_unstable_by(end - start, |a, b| a.cmp(b));
                }
                let page_slice = &mut matching[start..end];
                page_slice.sort_unstable_by(|a, b| a.cmp(b));
                page_slice.to_vec()
            } else {
                Vec::new()
            }
        };

        // Phase 2.5: hydration
        if page_items.is_empty() {
            return Vec::new();
        }

        let seq_set: std::collections::HashSet<u64> =
            page_items.iter().map(|item| item.seq).collect();
        let hydrated: std::collections::HashMap<u64, String> = col
            .iter()
            .filter_map(|entry| {
                let seq = read_msgpack_seq(entry.value());
                if seq_set.contains(&seq) {
                    Some((seq, entry.key().clone()))
                } else {
                    None
                }
            })
            .collect();

        // Phase 3: decode
        page_items
            .iter()
            .filter_map(|item| {
                let key = hydrated.get(&item.seq)?;
                let entry = col.get(key)?;
                let val = decode(entry.value())?;
                Some((key.clone(), val))
            })
            .collect()
    }
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
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;
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
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
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
        let Some(col) = state.get(collection) else {
            return Vec::new();
        };
        // Each rayon worker keeps its own bounded heap (size ≤ cap) and we merge
        // them at the end. Peak memory is O(workers * cap) instead of
        // O(collection size), and we avoid materialising a giant intermediate Vec
        // — which is the dominant cost for sort-only queries over 1M docs.
        let push_into = |heap: &mut BinaryHeap<HeapItem<C>>, k: String, v: Value, cmp: &Arc<C>| {
            let candidate = HeapItem {
                key: k,
                value: v,
                cmp: cmp.clone(),
            };
            if heap.len() >= cap {
                // Only evict the current worst if the candidate is strictly better.
                if let Some(worst) = heap.peek() {
                    if candidate.cmp(worst) != Ordering::Less {
                        return;
                    }
                }
            }
            heap.push(candidate);
            if heap.len() > cap {
                heap.pop();
            }
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
        merged
            .into_sorted_vec()
            .into_iter()
            .map(|h| (h.key, h.value))
            .collect()
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
                if !predicate(&v) {
                    continue;
                }
                let candidate = HeapItem {
                    key: entry.key().clone(),
                    value: v,
                    cmp: cmp.clone(),
                };
                if heap.len() >= cap {
                    if let Some(worst) = heap.peek() {
                        if candidate.cmp(worst) != Ordering::Less {
                            continue;
                        }
                    }
                }
                heap.push(candidate);
                if heap.len() > cap {
                    heap.pop();
                }
            }
        }
        heap.into_sorted_vec()
            .into_iter()
            .map(|h| (h.key, h.value))
            .collect()
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

        let limit = count.unwrap_or(usize::MAX);
        let deep = count.is_none() || (offset + limit > 5000);

        let page_items: Vec<CompactItem> = if !deep {
            use std::collections::BinaryHeap;
            let heap = col
                .par_iter()
                .fold(
                    || BinaryHeap::<CompactItem>::with_capacity(offset + limit + 1),
                    |mut h, entry| {
                        let seq = read_msgpack_seq(entry.value());
                        let item = CompactItem {
                            sort_value: seq as f64,
                            seq,
                        };
                        if h.len() >= offset + limit {
                            if let Some(worst) = h.peek() {
                                if item < *worst {
                                    h.pop();
                                    h.push(item);
                                }
                            }
                        } else {
                            h.push(item);
                        }
                        h
                    },
                )
                .reduce(
                    || BinaryHeap::<CompactItem>::with_capacity(offset + limit + 1),
                    |mut a, b| {
                        for item in b {
                            if a.len() >= offset + limit {
                                if let Some(worst) = a.peek() {
                                    if item < *worst {
                                        a.pop();
                                        a.push(item);
                                    }
                                }
                            } else {
                                a.push(item);
                            }
                        }
                        a
                    },
                );
            let sorted = heap.into_sorted_vec();
            let start = offset.min(sorted.len());
            sorted[start..].to_vec()
        } else {
            let mut matching: Vec<CompactItem> = col
                .par_iter()
                .map(|entry| {
                    let seq = read_msgpack_seq(entry.value());
                    CompactItem {
                        sort_value: seq as f64,
                        seq,
                    }
                })
                .collect();

            let total_len = matching.len();
            let start = offset;
            let end = match count {
                Some(l) => (offset + l).min(total_len),
                None => total_len,
            };

            if start < total_len {
                matching.select_nth_unstable_by(start, |a, b| a.cmp(b));
                if end < total_len {
                    matching[start..].select_nth_unstable_by(end - start, |a, b| a.cmp(b));
                }
                let page_slice = &mut matching[start..end];
                page_slice.sort_unstable_by(|a, b| a.cmp(b));
                page_slice.to_vec()
            } else {
                Vec::new()
            }
        };

        // Phase 2.5: hydration
        if page_items.is_empty() {
            return Vec::new();
        }

        use std::collections::HashSet;
        let seq_set: HashSet<u64> = page_items.iter().map(|item| item.seq).collect();
        use std::collections::HashMap;
        let hydrated: HashMap<u64, String> = col
            .par_iter()
            .filter_map(|entry| {
                let seq = read_msgpack_seq(entry.value());
                if seq_set.contains(&seq) {
                    Some((seq, entry.key().clone()))
                } else {
                    None
                }
            })
            .collect();

        // Phase 3: decode
        page_items
            .par_iter()
            .filter_map(|item| {
                let key = hydrated.get(&item.seq)?.clone();
                let entry = col.get(&key)?;
                let val = decode(entry.value())?;
                Some((key, val))
            })
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let col = match state.get(collection) {
            Some(c) => c,
            None => return Vec::new(),
        };

        let limit = count.unwrap_or(usize::MAX);
        let deep = count.is_none() || (offset + limit > 5000);

        let page_items: Vec<CompactItem> = if !deep {
            let mut heap = std::collections::BinaryHeap::with_capacity(offset + limit + 1);
            for entry in col.iter() {
                let seq = read_msgpack_seq(entry.value());
                let item = CompactItem {
                    sort_value: seq as f64,
                    seq,
                };
                if heap.len() >= offset + limit {
                    if let Some(worst) = heap.peek() {
                        if item < *worst {
                            heap.pop();
                            heap.push(item);
                        }
                    }
                } else {
                    heap.push(item);
                }
            }
            let sorted = heap.into_sorted_vec();
            let start = offset.min(sorted.len());
            sorted[start..].to_vec()
        } else {
            let mut matching: Vec<CompactItem> = col
                .iter()
                .map(|entry| {
                    let seq = read_msgpack_seq(entry.value());
                    CompactItem {
                        sort_value: seq as f64,
                        seq,
                    }
                })
                .collect();

            let total_len = matching.len();
            let start = offset;
            let end = match count {
                Some(l) => (offset + l).min(total_len),
                None => total_len,
            };

            if start < total_len {
                matching.select_nth_unstable_by(start, |a, b| a.cmp(b));
                if end < total_len {
                    matching[start..].select_nth_unstable_by(end - start, |a, b| a.cmp(b));
                }
                let page_slice = &mut matching[start..end];
                page_slice.sort_unstable_by(|a, b| a.cmp(b));
                page_slice.to_vec()
            } else {
                Vec::new()
            }
        };

        // Phase 2.5: hydration
        if page_items.is_empty() {
            return Vec::new();
        }

        let seq_set: std::collections::HashSet<u64> =
            page_items.iter().map(|item| item.seq).collect();
        let hydrated: std::collections::HashMap<u64, String> = col
            .iter()
            .filter_map(|entry| {
                let seq = read_msgpack_seq(entry.value());
                if seq_set.contains(&seq) {
                    Some((seq, entry.key().clone()))
                } else {
                    None
                }
            })
            .collect();

        // Phase 3: decode
        page_items
            .iter()
            .filter_map(|item| {
                let key = hydrated.get(&item.seq)?;
                let entry = col.get(key)?;
                let val = decode(entry.value())?;
                Some((key.clone(), val))
            })
            .collect()
    }
}
