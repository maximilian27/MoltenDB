// ttl_sweep.rs
// Background TTL eviction sweep for the MoltenDB server.
//
// Strategy: event-driven min-heap, not a fixed interval.
//
//   - On startup, scans all collections for documents with _expiresAt and
//     pre-populates the heap (up to MAX_HEAP_SIZE entries).
//   - Subscribes to the broadcast channel; adds documents with _expiresAt
//     to a min-heap ordered by expiry time.
//   - Sleeps until the next expiry, then performs O(1) targeted eviction:
//     fetch the exact document, verify it is still expired, delete it.
//   - When the heap is at capacity (MAX_HEAP_SIZE), falls back to a periodic
//     full-collection scan via delete_filtered to catch any overflow entries.
//   - Falls back to a 60-second idle sleep when the heap is empty.
//
// Properties:
//   - Zero CPU usage when there are no TTL documents.
//   - Sub-second eviction accuracy for short TTLs.
//   - No mindless polling.
//   - O(1) eviction per document when heap is not at capacity.
//   - Periodic fallback scan when heap overflows (bounded memory usage).

use moltendb_core::engine::{self, ttl};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::info;

/// Maximum number of entries in the min-heap.
/// Caps memory usage at ~4 MB regardless of collection size.
/// When exceeded, a periodic fallback scan handles the overflow.
const MAX_HEAP_SIZE: usize = 50_000;

/// Idle sleep duration when the heap is empty (seconds).
const IDLE_SECS: u64 = 60;

/// Fallback scan interval when the heap is at capacity (seconds).
/// Ensures overflow entries are still evicted even when the heap is full.
const FALLBACK_SCAN_SECS: u64 = 30;

/// A pending expiry entry in the min-heap.
#[derive(Eq, PartialEq)]
struct ExpiryEntry {
    /// Absolute Unix timestamp in milliseconds when this document expires.
    expires_at_ms: u64,
    /// Shared collection name -- Arc<str> avoids cloning the string per entry.
    collection: Arc<str>,
    /// The exact document key -- required for O(1) targeted eviction.
    key: String,
}

impl Ord for ExpiryEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Standard (ascending) comparison. The `Reverse` wrapper on the
        // BinaryHeap declaration turns this into a min-heap so the soonest
        // expiry is always at the top.
        self.expires_at_ms.cmp(&other.expires_at_ms)
    }
}

impl PartialOrd for ExpiryEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Parse a broadcast message and push an ExpiryEntry onto the heap if the
/// event carries a key and an expires_at_ms value. Respects MAX_HEAP_SIZE.
fn try_push(heap: &mut BinaryHeap<Reverse<ExpiryEntry>>, msg: &str) {
    if heap.len() >= MAX_HEAP_SIZE {
        return; // heap at capacity -- fallback scan will handle overflow
    }
    if let Ok(event) = serde_json::from_str::<serde_json::Value>(msg) {
        if event.get("event").and_then(|v| v.as_str()) == Some("change") {
            if let (Some(col), Some(key), Some(expires_at)) = (
                event.get("collection").and_then(|v| v.as_str()),
                event.get("key").and_then(|v| v.as_str()),
                event.get("expires_at_ms").and_then(|v| v.as_u64()),
            ) {
                heap.push(Reverse(ExpiryEntry {
                    expires_at_ms: expires_at,
                    collection: Arc::from(col),
                    key: key.to_string(),
                }));
            }
        }
    }
}

/// Spawn the background TTL sweep task.
pub fn spawn(db: engine::Db) {
    tokio::spawn(async move {
        run_sweep(db).await;
    });
}

async fn run_sweep(db: engine::Db) {
    let mut heap: BinaryHeap<Reverse<ExpiryEntry>> = BinaryHeap::new();
    let mut rx = db.subscribe();

    // -- Startup scan: pre-populate heap with existing TTL documents ----------
    let now = ttl::now_ms();
    let mut expiring = db.scan_expiring();
    // Sort by expiry so we fill the heap with the soonest-expiring entries first.
    expiring.sort_unstable_by_key(|(_, _, exp)| *exp);
    for (col, key, expires_at) in expiring {
        if heap.len() >= MAX_HEAP_SIZE { break; }
        // Already expired -- delete immediately rather than queuing.
        if expires_at <= now {
            let _ = db.delete(&col, vec![key]);
        } else {
            heap.push(Reverse(ExpiryEntry {
                expires_at_ms: expires_at,
                collection: Arc::from(col.as_str()),
                key,
            }));
        }
    }

    // Track when we last ran the fallback scan.
    let mut last_fallback = ttl::now_ms();

    loop {
        // -- Drain pending broadcast events (non-blocking) --------------------
        loop {
            match rx.try_recv() {
                Ok(msg) => try_push(&mut heap, &msg),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return,
            }
        }

        // -- Determine sleep duration -----------------------------------------
        let now = ttl::now_ms();

        // If heap is at capacity, cap sleep to FALLBACK_SCAN_SECS so we run
        // the fallback scan regularly even when no entries expire soon.
        let heap_at_cap = heap.len() >= MAX_HEAP_SIZE;

        let sleep_ms = if let Some(Reverse(next)) = heap.peek() {
            let until_next = if next.expires_at_ms <= now { 0 } else { next.expires_at_ms - now };
            if heap_at_cap {
                until_next.min(FALLBACK_SCAN_SECS * 1_000)
            } else {
                until_next
            }
        } else if heap_at_cap {
            FALLBACK_SCAN_SECS * 1_000
        } else {
            IDLE_SECS * 1_000
        };

        if sleep_ms > 0 {
            tokio::select! {
                _ = sleep(Duration::from_millis(sleep_ms)) => {},
                result = rx.recv() => {
                    if let Ok(msg) = result { try_push(&mut heap, &msg); }
                    continue;
                }
            }
        }

        // -- O(1) Targeted Eviction -------------------------------------------
        // Group expired keys by collection for efficient batch deletion.
        let now = ttl::now_ms();
        let mut to_delete: HashMap<String, Vec<String>> = HashMap::new();

        while let Some(Reverse(entry)) = heap.peek() {
            if entry.expires_at_ms <= now {
                let entry = heap.pop().unwrap().0;
                // Fetch the exact document to verify it is still expired.
                // The TTL may have been extended since this entry was enqueued.
                let result = db.get(&entry.collection, vec![entry.key.clone()]);
                if let Some(doc) = result.get(&entry.key) {
                    if ttl::is_expired(doc, now) {
                        to_delete.entry(entry.collection.to_string()).or_default().push(entry.key);
                    }
                }
            } else {
                break;
            }
        }

        // Issue targeted batch deletes -- no full-collection scan.
        for (col, keys) in to_delete {
            let count = keys.len();
            match db.delete(&col, keys) {
                Ok(_) => info!("TTL sweep: evicted {} expired document(s) from '{}'", count, col),
                Err(e) => tracing::warn!("TTL sweep error on '{}': {}", col, e),
            }
        }

        // -- Fallback scan (only when heap was at capacity) -------------------
        // Catches any TTL documents that were dropped from the heap due to the
        // MAX_HEAP_SIZE cap. Runs at most every FALLBACK_SCAN_SECS seconds.
        if heap_at_cap {
            let now = ttl::now_ms();
            if now - last_fallback >= FALLBACK_SCAN_SECS * 1_000 {
                last_fallback = now;
                // Scan all collections for expired documents.
                for col_ref in db.state_collections() {
                    let col = col_ref.to_string();
                    match db.delete_filtered(&col, move |doc| ttl::is_expired(doc, now), None) {
                        Ok(n) if n > 0 => info!("TTL fallback scan: evicted {} from '{}'", n, col),
                        _ => {}
                    }
                }
            }
        }
    }
}
