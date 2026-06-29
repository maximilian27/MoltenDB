// ─── Scope key pattern matching ──────────────────────────────────────────────

/// Match a scope key pattern against a concrete document key.
///
/// Supported patterns:
///   `"*"`         → matches any key (full wildcard)
///   `"store_A_*"` → matches any key starting with `"store_A_"` (prefix wildcard)
///   `"lp1"`       → exact match only
pub fn key_matches(pattern: &str, key: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return key.starts_with(prefix);
    }
    pattern == key
}

// ─── HMAC helpers ─────────────────────────────────────────────────────────────

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Compute an HMAC-SHA256 over `data` using `JWT_SECRET` as the key.
/// Returns the result as a lowercase hex string.
pub fn hmac_sign(data: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(crate::token::get_secret().as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

/// Verify an HMAC-SHA256 hex tag over `data` using `JWT_SECRET` as the key.
/// Returns `true` if the tag is valid, `false` otherwise.
/// Uses constant-time comparison internally (via the `hmac` crate) to prevent
/// timing attacks.
pub fn hmac_verify(data: &[u8], expected_hex: &str) -> bool {
    let Ok(expected_bytes) = hex::decode(expected_hex) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(crate::token::get_secret().as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(data);
    mac.verify_slice(&expected_bytes).is_ok()
}
