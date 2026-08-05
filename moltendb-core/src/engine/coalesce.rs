// ─── engine/coalesce.rs ───────────────────────────────────────────────────────
// Coalesced (batched) scanning for concurrent filtered GET queries.
//
// Problem: when many WHERE queries hit the same collection at nearly the same
// time, each one independently launches a full parallel pass over the whole
// collection. With N concurrent queries over M documents that is N full sweeps
// — every document's bytes are pulled from RAM into CPU cache N separate times
// and each query decodes/evaluates on its own. Under heavy read fan-out this
// pins every core doing largely redundant work.
//
// Solution (single-pass query batching / cooperative execution): a dedicated
// background coordinator thread collects incoming filtered-scan requests over a
// very short time window and then runs ONE shared pass over the collection. For
// each document the bytes are read once and every batched predicate is evaluated
// against them, so the 5M documents are streamed through cache a single time no
// matter how many queries are in the batch:
//
//   [Query 1] ─┐
//   [Query 2] ─┼─▶ [Coordinator] ─▶ single pass over docs ─▶ per-query results
//   [Query 3] ─┘
//
// Scope: this path handles full-collection scans that otherwise trigger a full
// parallel sweep per request — WHERE scans with no sort (the `WhereOnly`
// strategy in `handlers::get::fetch`) and single-field numeric-sort top-N scans
// (`process_get::run_fast_sort_path`, mirroring `operations::scan_top_n_raw`).
// Both kinds share the same window and the same pass: for each document the
// bytes are read once, every batched WHERE predicate is tested and every batched
// sort request feeds its own bounded heap. Point lookups, prefix scans, the
// multi-field/generic comparator sort path and the wasm build keep their
// existing code paths.
//
// Native-only: relies on Rayon and OS threads, neither of which exist on wasm32.
// ─────────────────────────────────────────────────────────────────────────────

use crate::common::system_field_tokens::{msgpack_to_value, read_msgpack_seq_token};
use dashmap::DashMap;
use rayon::prelude::*;
use serde_json::Value;
use std::collections::{BinaryHeap, HashMap};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The shared document store: collection name → (document key → MsgPack bytes).
type StateMap = DashMap<Arc<str>, DashMap<String, Box<[u8]>>>;

/// A boxed predicate evaluated directly on a document's raw MsgPack bytes.
/// Must be `Send + Sync` so the coordinator can share it across Rayon workers.
pub type ScanPredicate = Box<dyn Fn(&str, &[u8]) -> bool + Send + Sync>;

/// Time window during which incoming filtered-scan requests are batched before
/// a single shared pass over the collection is executed. Kept short so latency
/// stays low while still letting a burst of concurrent queries coalesce.
const BATCH_WINDOW: Duration = Duration::from_millis(3);

/// Maximum number of requests folded into one shared pass. Bounds the per-pass
/// work and memory; anything beyond this simply starts the next batch.
const MAX_BATCH: usize = 64;

/// What a batched request wants computed from the single shared pass.
enum ScanKind {
    /// Full-collection WHERE scan, results ordered by `_seq`. `need` is
    /// `offset + limit` (or `None` for "all matches"); pagination `offset` is
    /// applied by the caller. When `default_order_asc` is true matches are
    /// ordered by ascending `_seq` (oldest first), otherwise descending.
    Filtered {
        need: Option<usize>,
        default_order_asc: bool,
    },
    /// Single-field numeric-sort top-N (mirrors `operations::scan_top_n_raw`):
    /// during the pass the numeric `sort_field` is extracted from each matching
    /// document's raw bytes and fed into a bounded heap of size `cap`
    /// (= `offset + count`). `is_descending` reverses the order (largest first).
    SortedTopN {
        sort_field: String,
        is_descending: bool,
        cap: usize,
    },
}

/// A single scan request handed to the coordinator.
struct ScanRequest {
    /// Target collection name.
    collection: String,
    /// Predicate evaluated on each document's raw bytes.
    predicate: ScanPredicate,
    /// What to compute from the shared pass (WHERE ordering vs. sorted top-N).
    kind: ScanKind,
    /// Result channel: the coordinator sends the decoded matches back here.
    resp: Sender<Vec<(String, Value)>>,
}

/// A compact heap item used by the sorted top-N path: the document key plus the
/// numeric sort primitive extracted from its raw bytes. Mirrors
/// `operations::get::KeyedCompactItem` but kept local so the coordinator stays
/// self-contained. `Ord` breaks ties by `key` so eviction is deterministic.
#[derive(Clone)]
struct SortItem {
    key: Arc<str>,
    sort_value: f64,
}
impl PartialEq for SortItem {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.sort_value.total_cmp(&other.sort_value) == std::cmp::Ordering::Equal
            && self.key == other.key
    }
}
impl Eq for SortItem {}
impl PartialOrd for SortItem {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SortItem {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_value
            .total_cmp(&other.sort_value)
            .then_with(|| self.key.cmp(&other.key))
    }
}

/// Push `item` into a `cap`-bounded max-heap, keeping only the `cap` smallest
/// items (the worst/largest is evicted). Shared by the fold and reduce phases.
#[inline]
fn push_bounded(heap: &mut BinaryHeap<SortItem>, item: SortItem, cap: usize) {
    if cap == 0 {
        return;
    }
    if heap.len() >= cap {
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

/// Per-request work descriptor borrowed into the shared-pass closures. Only the
/// predicate (Send + Sync) and plain sort metadata are captured — never the full
/// `ScanRequest` (its result `Sender` is not `Sync`).
struct Task<'a> {
    predicate: &'a ScanPredicate,
    kind: TaskKind<'a>,
}

enum TaskKind<'a> {
    Filtered,
    Sorted {
        path_parts: Vec<&'a str>,
        is_descending: bool,
        cap: usize,
    },
}

/// Per-request accumulator for the shared pass: `_seq`-tagged matches for WHERE
/// requests, or a bounded heap for sorted top-N requests.
enum Accum {
    Filtered(Vec<(u64, String)>),
    Sorted(BinaryHeap<SortItem>),
}

/// Build a fresh set of accumulators matching each task's kind. Called by both
/// the Rayon `fold` and `reduce` identity closures.
fn new_accumulators(tasks: &[Task<'_>]) -> Vec<Accum> {
    tasks
        .iter()
        .map(|t| match &t.kind {
            TaskKind::Filtered => Accum::Filtered(Vec::new()),
            TaskKind::Sorted { cap, .. } => Accum::Sorted(BinaryHeap::with_capacity(cap + 1)),
        })
        .collect()
}

/// Handle used by `Db` to submit filtered scans to the background coordinator.
///
/// Cloning a `Db` shares the same coordinator; the `Sender` lives behind a
/// `Mutex` so the whole handle stays `Sync` (a bare `std::sync::mpsc::Sender`
/// is not `Sync`). The lock is held only for the duration of a single enqueue.
pub struct CoalescedScanner {
    tx: Mutex<Sender<ScanRequest>>,
}

impl CoalescedScanner {
    /// Spawn the coordinator thread and return a handle to it.
    ///
    /// The coordinator holds its own `Arc` clone of the shared state, so it
    /// keeps running for as long as any `CoalescedScanner` handle is alive.
    /// When the last handle (i.e. the last `Db` clone) is dropped the channel
    /// closes and the coordinator thread exits.
    pub fn new(state: Arc<StateMap>) -> Self {
        let (tx, rx) = mpsc::channel::<ScanRequest>();
        std::thread::Builder::new()
            .name("moltendb-coalesce".to_string())
            .spawn(move || coordinator_loop(rx, state))
            .expect("failed to spawn coalesced-scan coordinator thread");
        CoalescedScanner {
            tx: Mutex::new(tx),
        }
    }

    /// Submit a filtered scan and block until the shared pass returns its
    /// matches. `need` is `offset + limit` (the pagination `offset` itself is
    /// applied by the caller). Returns the matching `(key, value)` pairs
    /// ordered by `_seq` per `default_order_asc`.
    ///
    /// The returned values are decoded documents; the caller is responsible for
    /// any further expansion (e.g. `expand_system_fields`).
    pub fn scan(
        &self,
        collection: &str,
        predicate: ScanPredicate,
        need: Option<usize>,
        default_order_asc: bool,
    ) -> Vec<(String, Value)> {
        let (resp_tx, resp_rx) = mpsc::channel();
        let request = ScanRequest {
            collection: collection.to_string(),
            predicate,
            kind: ScanKind::Filtered {
                need,
                default_order_asc,
            },
            resp: resp_tx,
        };
        self.submit(request, resp_rx)
    }

    /// Submit a single-field numeric-sort top-N scan and block until the shared
    /// pass returns the top `cap` (= `offset + limit`) matches, ordered
    /// best-first per `is_descending`. Mirrors `operations::scan_top_n_raw`; the
    /// caller applies pagination `offset` and any shaping. Returns decoded
    /// `(key, value)` winners (the caller expands system fields).
    pub fn scan_top_n(
        &self,
        collection: &str,
        predicate: ScanPredicate,
        sort_field: &str,
        is_descending: bool,
        cap: usize,
    ) -> Vec<(String, Value)> {
        let (resp_tx, resp_rx) = mpsc::channel();
        let request = ScanRequest {
            collection: collection.to_string(),
            predicate,
            kind: ScanKind::SortedTopN {
                sort_field: sort_field.to_string(),
                is_descending,
                cap,
            },
            resp: resp_tx,
        };
        self.submit(request, resp_rx)
    }

    /// Enqueue a request and block until its result comes back. If the
    /// coordinator is gone there is nothing to batch against — return empty
    /// rather than panicking.
    fn submit(
        &self,
        request: ScanRequest,
        resp_rx: Receiver<Vec<(String, Value)>>,
    ) -> Vec<(String, Value)> {
        {
            let guard = match self.tx.lock() {
                Ok(g) => g,
                Err(_) => return Vec::new(),
            };
            if guard.send(request).is_err() {
                return Vec::new();
            }
        }
        resp_rx.recv().unwrap_or_default()
    }
}

/// Coordinator loop: waits for the first request, collects more for a short
/// window (bounded by `MAX_BATCH`), then runs one shared pass per collection.
fn coordinator_loop(rx: Receiver<ScanRequest>, state: Arc<StateMap>) {
    loop {
        // Block until at least one request arrives (or the channel closes).
        let first = match rx.recv() {
            Ok(req) => req,
            Err(_) => return, // all senders dropped — shut down.
        };

        let mut batch: Vec<ScanRequest> = Vec::with_capacity(MAX_BATCH);
        batch.push(first);

        // Collect additional requests until the window elapses or the batch
        // reaches its cap.
        let deadline = Instant::now() + BATCH_WINDOW;
        while batch.len() < MAX_BATCH {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(req) => batch.push(req),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        // Group by collection: each collection gets its own single shared pass.
        let mut groups: HashMap<String, Vec<ScanRequest>> = HashMap::new();
        for req in batch {
            groups.entry(req.collection.clone()).or_default().push(req);
        }
        for (collection, requests) in groups {
            run_batch_for_collection(&state, &collection, requests);
        }
    }
}

/// Execute a single shared pass over `collection`, evaluating every request's
/// predicate against each document exactly once. WHERE requests accumulate their
/// `_seq`-ordered matches; sorted top-N requests feed a bounded heap keyed on
/// the numeric sort field. Each request is then answered with its own ordered,
/// paginated, decoded result set.
fn run_batch_for_collection(state: &StateMap, collection: &str, requests: Vec<ScanRequest>) {
    let col = match state.get(collection) {
        Some(c) => c,
        None => {
            // Unknown collection — every request gets an empty result.
            for req in requests {
                let _ = req.resp.send(Vec::new());
            }
            return;
        }
    };

    // Per-request work descriptors borrowed into the parallel closures. Only the
    // predicate (Send + Sync) and plain sort metadata are captured — never the
    // full `ScanRequest` (its result `Sender` is not `Sync`).
    let tasks: Vec<Task<'_>> = requests
        .iter()
        .map(|r| match &r.kind {
            ScanKind::Filtered { .. } => Task {
                predicate: &r.predicate,
                kind: TaskKind::Filtered,
            },
            ScanKind::SortedTopN {
                sort_field,
                is_descending,
                cap,
            } => Task {
                predicate: &r.predicate,
                kind: TaskKind::Sorted {
                    path_parts: sort_field.split('.').collect(),
                    is_descending: *is_descending,
                    cap: *cap,
                },
            },
        })
        .collect();

    // ── Single shared pass ──────────────────────────────────────────────────
    // For each document read its bytes once and update every request's
    // accumulator. `_seq` is read lazily (once per doc, only if some WHERE
    // predicate matched it).
    let merged: Vec<Accum> = col
        .par_iter()
        .fold(
            || new_accumulators(&tasks),
            |mut acc, entry| {
                let key = entry.key();
                let bytes = entry.value();
                let mut seq: Option<u64> = None;
                for (i, task) in tasks.iter().enumerate() {
                    match &task.kind {
                        TaskKind::Filtered => {
                            if (task.predicate)(key, bytes) {
                                let s = *seq.get_or_insert_with(|| read_msgpack_seq_token(bytes));
                                if let Accum::Filtered(v) = &mut acc[i] {
                                    v.push((s, key.clone()));
                                }
                            }
                        }
                        TaskKind::Sorted {
                            path_parts,
                            is_descending,
                            cap,
                        } => {
                            if (task.predicate)(key, bytes) {
                                if let Some(slice) =
                                    crate::query::find_msgpack_value(bytes, path_parts)
                                {
                                    if let Some(num) = crate::query::read_msgpack_number(slice) {
                                        let sort_value = if *is_descending { -num } else { num };
                                        let item = SortItem {
                                            key: Arc::from(key.as_str()),
                                            sort_value,
                                        };
                                        if let Accum::Sorted(heap) = &mut acc[i] {
                                            push_bounded(heap, item, *cap);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                acc
            },
        )
        .reduce(
            || new_accumulators(&tasks),
            |mut a, b| {
                for (i, acc_b) in b.into_iter().enumerate() {
                    match (&mut a[i], acc_b) {
                        (Accum::Filtered(va), Accum::Filtered(mut vb)) => va.append(&mut vb),
                        (Accum::Sorted(ha), Accum::Sorted(hb)) => {
                            if let TaskKind::Sorted { cap, .. } = &tasks[i].kind {
                                for item in hb {
                                    push_bounded(ha, item, *cap);
                                }
                            }
                        }
                        // Accumulator variants always match their task kind.
                        _ => {}
                    }
                }
                a
            },
        );

    // ── Per-request ordering, pagination and decoding ───────────────────────
    for (req, acc) in requests.into_iter().zip(merged.into_iter()) {
        let out: Vec<(String, Value)> = match (req.kind, acc) {
            // WHERE: order matches by `_seq`, cap to `need`, decode winners.
            // Mirrors the WHERE branch of `operations::get::get_filtered`.
            (
                ScanKind::Filtered {
                    need,
                    default_order_asc,
                },
                Accum::Filtered(mut pairs),
            ) => {
                if default_order_asc {
                    pairs.sort_unstable_by_key(|(seq, _)| *seq);
                } else {
                    pairs.sort_unstable_by_key(|(seq, _)| std::cmp::Reverse(*seq));
                }
                let need = need.unwrap_or(usize::MAX);
                pairs
                    .into_iter()
                    .take(need)
                    .filter_map(|(_, key)| {
                        let entry = col.get(&key)?;
                        let val = msgpack_to_value(entry.value())?;
                        Some((key, val))
                    })
                    .collect()
            }
            // Sorted top-N: the bounded heap already holds the `cap` winners;
            // emit them best-first (ascending by stored `sort_value`, which was
            // negated for descending queries) and decode.
            (ScanKind::SortedTopN { .. }, Accum::Sorted(heap)) => heap
                .into_sorted_vec()
                .into_iter()
                .filter_map(|item| {
                    let entry = col.get(item.key.as_ref())?;
                    let val = msgpack_to_value(entry.value())?;
                    Some((item.key.to_string(), val))
                })
                .collect(),
            // A kind/accumulator mismatch cannot occur (built in lockstep).
            _ => Vec::new(),
        };
        let _ = req.resp.send(out);
    }
}
