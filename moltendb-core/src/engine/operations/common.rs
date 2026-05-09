// ─── operations/common.rs ─────────────────────────────────────────────────────
// Shared utilities used across all operation modules.
// ─────────────────────────────────────────────────────────────────────────────

/// Returns the current UTC time as an ISO 8601 string (e.g. "2026-03-04T21:58:00Z").
///
/// Used to stamp `_v`, `createdAt`, and `modifiedAt` on every document write.
/// Uses `web-time` for WASM compatibility, `std::time` on native.
pub fn now_iso() -> String {
    use web_time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (s, m, h) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
    let mut d = secs / 86400; let mut y = 1970u64;
    loop { let dy = if (y.is_multiple_of(4) && !y.is_multiple_of(100))||y.is_multiple_of(400){366}else{365}; if d<dy{break;} d-=dy; y+=1; }
    let lp = (y.is_multiple_of(4)&&!y.is_multiple_of(100))||y.is_multiple_of(400);
    let md:[u64;12]=[31,if lp{29}else{28},31,30,31,30,31,31,30,31,30,31];
    let mut mo=1u64; for &x in &md{if d<x{break;} d-=x; mo+=1;}
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",y,mo,d+1,h,m,s)
}
