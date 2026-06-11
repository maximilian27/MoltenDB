// ─── JWT helpers ──────────────────────────────────────────────────────────────

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::store::RevocationStore;
use crate::types::{AuthError, Claims};

/// The JWT signing secret, read once at startup from the JWT_SECRET environment
/// variable and cached for the lifetime of the process.
///
/// Using OnceLock avoids acquiring the global OS env-var lock on every request.
/// Falls back to a hardcoded default if the variable is not set.
///
/// WARNING: The default secret is publicly known — anyone can forge tokens
/// signed with it. Always set JWT_SECRET in production.
static JWT_SECRET: OnceLock<String> = OnceLock::new();

pub(crate) fn get_secret() -> &'static str {
    JWT_SECRET.get_or_init(|| {
        std::env::var("JWT_SECRET")
            .unwrap_or_else(|_| "dev-secret-change-in-production".to_string())
    })
}

/// Create a signed JWT token for the given username.
///
/// The token expires after `ttl_secs` seconds from now (default: 86400 = 24 hours).
/// Returns the compact serialization: "header.payload.signature"
pub fn create_token(username: &str, ttl_secs: u64) -> Result<String, AuthError> {
    create_scoped_token(username, vec!["*:*:*".to_string()], ttl_secs).map(|(token, _)| token)
}

/// Create a scoped delegate token with a custom TTL (in seconds).
///
/// Used by POST /auth/delegate to mint narrow-permission tokens for clients.
/// The root user calls this on behalf of a client; the client never sees the
/// root credentials.
///
/// Returns `(token_string, jti)` — the jti is included in the DelegateResponse
/// so callers can revoke the token via DELETE /auth/tokens/:jti without having
/// to decode the JWT themselves.
pub fn create_scoped_token(
    username: &str,
    scopes: Vec<String>,
    ttl_secs: u64,
) -> Result<(String, String), AuthError> {
    let expiration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + ttl_secs;

    let jti = Uuid::new_v4().to_string();

    let claims = Claims {
        sub: username.to_string(),
        exp: expiration,
        scopes,
        jti: jti.clone(),
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(get_secret().as_bytes()),
    )
    .map_err(|e| {
        if e.kind() == &jsonwebtoken::errors::ErrorKind::ExpiredSignature {
            AuthError::TokenExpired
        } else {
            AuthError::InvalidToken(e)
        }
    })?;

    Ok((token, jti))
}

/// Verify a JWT token and return the decoded Claims if valid.
///
/// This checks:
///   1. The signature — was this token signed with our secret?
///   2. The expiry — has the token expired?
///
/// Returns Err if the token is invalid, expired, or malformed.
pub fn verify_token(token: &str) -> Result<Claims, AuthError> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(get_secret().as_bytes()),
        // Validation::default() checks signature + expiry automatically.
        &Validation::default(),
    )
    .map_err(|e| {
        if e.kind() == &jsonwebtoken::errors::ErrorKind::ExpiredSignature {
            AuthError::TokenExpired
        } else {
            AuthError::InvalidToken(e)
        }
    })?;
    Ok(token_data.claims)
}

// ─── Token refresh ────────────────────────────────────────────────────────────

/// Refresh a scoped token. Returns a new `(token, jti)` pair with the same
/// `sub` and `scopes` but a fresh `exp` and a new `jti`.
///
/// Rules:
/// - The old token must be valid (non-expired, valid signature, not revoked).
/// - Root tokens (`*:*:*`) are **not** refreshable — returns
///   `Err(AuthError::RefreshNotAllowed)`.
/// - The old `jti` is immediately added to the `RevocationStore` so the old
///   token cannot be replayed after a successful refresh.
///
/// The caller is responsible for persisting the revocation store after this
/// call (e.g. via `revocation_store.save_to_file(...).await`).
pub fn refresh_scoped_token(
    old_token: &str,
    new_ttl_secs: u64,
    revocation_store: &RevocationStore,
) -> Result<(String, String), AuthError> {
    use std::time::Instant;

    // Verify signature + expiry.
    let claims = verify_token(old_token)?;

    // Fail-closed: reject tokens without a jti (cannot be revoked).
    if claims.jti.is_empty() {
        return Err(AuthError::InvalidToken(jsonwebtoken::errors::Error::from(
            jsonwebtoken::errors::ErrorKind::InvalidToken,
        )));
    }

    // Root tokens must not be refreshable — intentional friction.
    if claims.is_admin() {
        return Err(AuthError::RefreshNotAllowed);
    }

    // Check the revocation store — refuse to refresh an already-revoked token.
    if revocation_store.is_revoked(&claims.jti) {
        return Err(AuthError::TokenRevoked);
    }

    // Mint the new token with the same sub + scopes but a fresh exp + jti.
    let (new_token, new_jti) =
        create_scoped_token(&claims.sub, claims.scopes.clone(), new_ttl_secs)?;

    // Revoke the old jti immediately — prevents replay attacks.
    // Prune deadline = now + new_ttl_secs (safe upper bound; the old token
    // would have expired within its original TTL anyway, but we use the new
    // TTL as a conservative upper bound for the prune deadline).
    let prune_after = Instant::now() + std::time::Duration::from_secs(new_ttl_secs);
    revocation_store.revoke(&claims.jti, prune_after);

    Ok((new_token, new_jti))
}
