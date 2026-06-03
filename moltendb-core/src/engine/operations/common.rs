// ─── operations/document_processing ─────────────────────────────────────────────────────
// Shared utilities used across all operation modules.
// ─────────────────────────────────────────────────────────────────────────────

/// Returns the current time as Unix milliseconds (u64).
///
/// Used to stamp `_createdAt` and `_modifiedAt` on every document write.
/// Uses `web-time` for WASM compatibility, `std::time` on native.
pub fn now_unix_ms() -> u64 {
    use web_time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
