// u2500u2500u2500 operations/ttl.rs u2500u2500u2500
// TTL (time-to-live) helpers.
//
// Responsibilities:
//   - `apply_ttl`   -- called by process_set / process_update to inject
//                     `_expiresAt` when the collection has a TTL default.
//   - `is_expired`  -- called by shape_doc for lazy read-time eviction.
//   - `now_ms`      -- shared helper: current Unix time in milliseconds.
// u2500u2500u2500

use dashmap::DashMap;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the current Unix timestamp in milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Inject `_expiresAt` (absolute Unix ms) if the collection has a TTL default.
///
/// TTL is collection-level only -- per-document `_ttl` is not supported.
/// If the collection has no TTL default the document is left unchanged.
/// Expiry is always calculated from the current time (`now_ms()`).
pub fn apply_ttl(
    doc: &mut serde_json::Map<String, Value>,
    collection: &str,
    ttl_defaults: &DashMap<String, u64>,
) {
    if let Some(secs) = ttl_defaults.get(collection).map(|v| *v) {
        let expires_at = now_ms() + secs * 1_000;
        doc.insert("_expiresAt".to_string(), Value::Number(expires_at.into()));
    }
}

/// Returns `true` if the document has an `_expiresAt` field whose value is
/// in the past (i.e. the document has expired and should be treated as gone).
///
/// PERFORMANCE: `current_time_ms` must be passed in by the caller -- do NOT
/// call `now_ms()` inside this function. It is used in tight loops over
/// potentially millions of documents (rayon parallel scans) and calling
/// `SystemTime::now()` per document would cause severe syscall bottlenecks.
/// The caller should invoke `now_ms()` exactly once and pass the result here.
#[inline]
pub fn is_expired(doc: &Value, current_time_ms: u64) -> bool {
    if let Some(expires) = doc.get("_expiresAt").and_then(|v| v.as_u64()) {
        return current_time_ms >= expires;
    }
    false
}
