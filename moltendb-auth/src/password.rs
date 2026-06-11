// ─── Password hashing ─────────────────────────────────────────────────────────
// bcrypt is a deliberately slow password hashing algorithm — it's designed to
// make brute-force attacks expensive even if the hash database is leaked.
// DEFAULT_COST = 12 iterations (takes ~250ms on modern hardware).

use crate::types::AuthError;

/// Hash a plaintext password using bcrypt. Returns the hash string.
/// Store the hash, never the plaintext password.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST).map_err(AuthError::HashError)
}

/// Verify a plaintext password against a stored bcrypt hash.
/// Returns true if the password matches, false otherwise.
pub fn verify_password(password: &str, hash: &str) -> Result<bool, AuthError> {
    bcrypt::verify(password, hash).map_err(AuthError::HashError)
}
