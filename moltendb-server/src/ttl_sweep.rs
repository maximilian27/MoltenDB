// ttl_sweep.rs
// Background TTL eviction sweep for the MoltenDB server.
//
// Strategy: event-driven min-heap with one entry per collection.
//
//   - On startup, reads all (collection, expires_at_ms) pairs from the engine
//     and pre-populates the heap.
//   - Subscribes to the broadcast channel; on every `ttl_expiry` event the
//     heap entry for that collection is updated (or added).
//   - Sleeps until the next collection expiry, then drops the entire collection
//     in one O(1) call -- no per-document iteration.
//   - Falls back to a 60-second idle sleep when the heap is empty.
//
// Properties:
//   - Zero CPU usage when there are no TTL collections.
//   - Sub-second eviction accuracy for short TTLs.
//   - O(1) eviction per collection (drop entire collection at once).
//   - No per-document scanning -- heap has at most one entry per collection.

use moltendb_core::engine::{self, ttl};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use tokio::time::{sleep, Duration};
use tracing::info;

/// One entry per collection in the min-heap.
#[derive(Eq, PartialEq)]
struct ExpiryEntry {
    /// Absolute Unix timestamp in milliseconds when this collection expires.
    expires_at_ms: u64,
    /// Collection name.
    collection: String,
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

/// Spawn the background TTL sweep task.
pub fn spawn(db: engine::Db) {
    tokio::spawn(async move {
        run_sweep(db).await;
    });
}

async fn run_sweep(db: engine::Db) {
    let mut heap: BinaryHeap<Reverse<ExpiryEntry>> = BinaryHeap::new();
    let mut rx = db.subscribe();

    // -- Startup: pre-populate heap from existing TTL expiry map -------------
    let now = ttl::now_ms();
    let mut expiries = db.all_ttl_expiries();
    expiries.sort_unstable_by_key(|(_, exp)| *exp);
    for (col, expires_at) in expiries {
        if expires_at <= now {
            // Already expired -- drop immediately.
            match db.delete_collection(&col) {
                Ok(_) => info!("TTL sweep (startup): dropped expired collection '{}'", col),
                Err(e) => tracing::warn!("TTL sweep startup error on '{}': {}", col, e),
            }
        } else {
            heap.push(Reverse(ExpiryEntry { expires_at_ms: expires_at, collection: col }));
        }
    }

    const IDLE_SECS: u64 = 60;

    loop {
        // -- Drain pending broadcast events (non-blocking) --------------------
        loop {
            match rx.try_recv() {
                Ok(msg) => try_update_heap(&mut heap, &msg),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return,
            }
        }

        // -- Determine sleep duration -----------------------------------------
        let now = ttl::now_ms();
        let sleep_ms = if let Some(Reverse(next)) = heap.peek() {
            if next.expires_at_ms <= now { 0 } else { next.expires_at_ms - now }
        } else {
            IDLE_SECS * 1_000
        };

        if sleep_ms > 0 {
            tokio::select! {
                _ = sleep(Duration::from_millis(sleep_ms)) => {},
                result = rx.recv() => {
                    if let Ok(msg) = result { try_update_heap(&mut heap, &msg); }
                    continue;
                }
            }
        }

        // -- Eviction: drop all collections whose expiry has passed -----------
        let now = ttl::now_ms();
        while let Some(Reverse(entry)) = heap.peek() {
            if entry.expires_at_ms <= now {
                let entry = heap.pop().unwrap().0;
                // Verify the expiry is still current -- it may have been
                // extended by a new insert since this entry was enqueued.
                match db.get_ttl_expiry(&entry.collection) {
                    Some(current_exp) if current_exp > now => {
                        // TTL was extended -- re-queue with the updated expiry.
                        heap.push(Reverse(ExpiryEntry {
                            expires_at_ms: current_exp,
                            collection: entry.collection,
                        }));
                    }
                    Some(_) | None => {
                        // Still expired (or TTL was removed) -- drop the collection.
                        match db.delete_collection(&entry.collection) {
                            Ok(_) => info!("TTL sweep: dropped expired collection '{}'", entry.collection),
                            Err(e) => tracing::warn!("TTL sweep error on '{}': {}", entry.collection, e),
                        }
                    }
                }
            } else {
                break;
            }
        }
    }
}

/// Parse a broadcast message and update the heap entry for the collection
/// if the event is a `ttl_expiry` event.
fn try_update_heap(heap: &mut BinaryHeap<Reverse<ExpiryEntry>>, msg: &str) {
    if let Ok(event) = serde_json::from_str::<serde_json::Value>(msg) {
        if event.get("event").and_then(|v| v.as_str()) == Some("ttl_expiry") {
            if let (Some(col), Some(expires_at)) = (
                event.get("collection").and_then(|v| v.as_str()),
                event.get("expires_at_ms").and_then(|v| v.as_u64()),
            ) {
                // Push the new entry. Stale entries for the same collection
                // will be skipped at eviction time via the `get_ttl_expiry` verify step.
                heap.push(Reverse(ExpiryEntry {
                    expires_at_ms: expires_at,
                    collection: col.to_string(),
                }));
            }
        }
    }
}
