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

/// Converts a Unix timestamp in milliseconds to an ISO 8601 string (e.g. "2026-03-04T21:58:00Z").
/// Matches the format used by `_createdAt` and `_modifiedAt`.
pub fn ms_to_iso(ms: u64) -> String {
    let secs = ms / 1_000;
    let (s, m, h) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
    let mut d = secs / 86400;
    let mut y = 1970u64;
    loop {
        let dy = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
        if d < dy { break; }
        d -= dy;
        y += 1;
    }
    let lp = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let md: [u64; 12] = [31, if lp { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 1u64;
    for &x in &md { if d < x { break; } d -= x; mo += 1; }
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d + 1, h, m, s)
}

/// Parses an ISO 8601 string (e.g. "2026-03-04T21:58:00Z") back to Unix milliseconds.
/// Returns `None` if the string cannot be parsed.
pub(crate) fn iso_to_ms(iso: &str) -> Option<u64> {
    // Expected format: "YYYY-MM-DDTHH:MM:SSZ"
    let b = iso.as_bytes();
    if b.len() < 20 { return None; }
    let y: u64 = std::str::from_utf8(&b[0..4]).ok()?.parse().ok()?;
    let mo: u64 = std::str::from_utf8(&b[5..7]).ok()?.parse().ok()?;
    let d: u64 = std::str::from_utf8(&b[8..10]).ok()?.parse().ok()?;
    let h: u64 = std::str::from_utf8(&b[11..13]).ok()?.parse().ok()?;
    let mi: u64 = std::str::from_utf8(&b[14..16]).ok()?.parse().ok()?;
    let s: u64 = std::str::from_utf8(&b[17..19]).ok()?.parse().ok()?;
    // Days since epoch
    let mut days: u64 = 0;
    for yr in 1970..y {
        days += if (yr % 4 == 0 && yr % 100 != 0) || yr % 400 == 0 { 366 } else { 365 };
    }
    let lp = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let md: [u64; 12] = [31, if lp { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for i in 0..(mo as usize - 1) { days += md[i]; }
    days += d - 1;
    let secs = days * 86400 + h * 3600 + mi * 60 + s;
    Some(secs * 1_000)
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
        let expires_at_ms = now_ms() + secs * 1_000;
        doc.insert("_expiresAt".to_string(), Value::String(ms_to_iso(expires_at_ms)));
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
    if let Some(expires_ms) = doc.get("_expiresAt").and_then(|v| v.as_str()).and_then(iso_to_ms) {
        return current_time_ms >= expires_ms;
    }
    false
}
