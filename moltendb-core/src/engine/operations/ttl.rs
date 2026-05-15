// operations/ttl.rs
// TTL (time-to-live) helpers -- collection-level only.
//
// Design: TTL is stored at collection level, not per document.
// When a collection has a TTL, its absolute expiry timestamp (Unix ms) is
// stored in `Db::ttl_expiry`. The expiry is (re)computed from `now_ms()` at
// the end of every insert batch -- so the clock starts when the last write
// of that batch commits, not when the schema was registered.
//
// `_expiresAt` is a virtual field: it is never stored inside documents.
// It is computed on read from the collection expiry map and returned to
// clients in the same ISO 8601 format as `_createdAt` / `_modifiedAt`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the current Unix timestamp in milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Converts a Unix timestamp in milliseconds to an ISO 8601 string
/// (e.g. "2026-03-04T21:58:00Z"). Matches the format used by
/// `_createdAt` and `_modifiedAt`.
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

/// Returns `true` if the collection has expired.
///
/// `expires_at_ms` is the absolute Unix ms timestamp stored in `Db::ttl_expiry`.
/// `current_time_ms` must be captured once by the caller (never call `now_ms()`
/// inside a per-document loop).
#[inline]
pub fn collection_is_expired(expires_at_ms: u64, current_time_ms: u64) -> bool {
    current_time_ms >= expires_at_ms
}
