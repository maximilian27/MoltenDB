#![cfg(not(target_arch = "wasm32"))]
#![deny(warnings)]
// ─── auth.rs ──────────────────────────────────────────────────────────────────
// This file implements authentication and authorisation for the MoltenDB server.
//
// Two mechanisms are used together:
//
//   1. Password authentication (login endpoint)
//      The client sends a username + password to POST /login.
//      The server verifies the password against a bcrypt hash stored in memory.
//      On success, a signed JWT token is returned.
//
//   2. JWT bearer token (protected endpoints)
//      Every request to a protected route must include an Authorization header:
//        Authorization: Bearer <token>
//      The auth_middleware Axum layer verifies the token's signature and expiry.
//      If valid, the decoded Claims are attached to the request extensions so
//      downstream handlers can read the authenticated username.
//
// User storage:
//   Users are stored in an in-memory DashMap (username → bcrypt hash).
//   Credentials are loaded from environment variables at startup:
//     MOLTENDB_ROOT_USER
//     MOLTENDB_ROOT_PASSWORD
//   No external database is needed. Additional users can be added at runtime
//   via UserStore::add_user().
//
// JWT:
//   Tokens are signed with HMAC-SHA256 using a secret from JWT_SECRET env var.
//   Tokens expire after 24 hours.
// ─────────────────────────────────────────────────────────────────────────────

// moltendb-auth is a native-only crate — it is never compiled for WASM targets.

// Axum types for building middleware.
use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
// HMAC-SHA256 for signing the revocation file.
use hmac::{Hmac, Mac};
// JWT encoding/decoding functions and types.
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
// Serde traits for serializing/deserializing the Claims struct to/from JSON.
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
// OnceLock for reading JWT_SECRET exactly once at startup.
use std::sync::OnceLock;
// SystemTime and UNIX_EPOCH for computing token expiry timestamps.
use std::time::{Instant, SystemTime, UNIX_EPOCH};
// UUID for generating unique JWT IDs (jti).
use uuid::Uuid;

// ─── JWT data structures ──────────────────────────────────────────────────────

/// The payload embedded inside a JWT token.
///
/// When a token is created, these fields are serialized to JSON, base64-encoded,
/// and signed. When a token is verified, the signature is checked and these
/// fields are decoded back.
///
/// Standard JWT claim names:
///   `sub` = "subject" — who the token was issued to (the username).
///   `exp` = "expiration" — Unix timestamp after which the token is invalid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// The authenticated username (e.g. "admin").
    pub sub: String,
    /// Token expiry as a Unix timestamp (seconds since 1970-01-01 00:00:00 UTC).
    pub exp: u64,
    /// Scopes granted to this token.
    /// Format: "action:collection:document_key"
    /// Examples: "read:laptops:lp1", "write:users:*", "read:*:*", "*:*:*"
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Unique JWT ID — used for token revocation.
    /// Required on all tokens minted by this crate. Tokens without a jti are
    /// rejected by auth_middleware (fail-closed — no revocation bypass).
    pub jti: String,
}

impl Claims {
    /// Check whether this token grants access for a given action on a
    /// specific collection + document key.
    ///
    /// Evaluation order (most-specific first):
    ///   1. "*:*:*"                   → always grants everything (root/admin)
    ///   2. "action:*:*"               → global wildcard for this action
    ///   3. "action:collection:*"      → all docs in this collection
    ///   4. "action:collection:key"    → exact document match
    pub fn has_access(&self, action: &str, collection: &str, doc_key: &str) -> bool {
        self.scopes.iter().any(|scope| {
            if scope == "*:*:*" {
                return true;
            }
            let parts: Vec<&str> = scope.splitn(3, ':').collect();
            if parts.len() != 3 {
                return false;
            }
            let (s_action, s_col, s_key) = (parts[0], parts[1], parts[2]);
            let action_match = s_action == action;
            let col_match = s_col == "*" || s_col == collection;
            let key_match = key_matches(s_key, doc_key);
            action_match && col_match && key_match
        })
    }

    /// Convenience: check collection-level access (key wildcard).
    pub fn has_collection_access(&self, action: &str, collection: &str) -> bool {
        self.has_access(action, collection, "*")
    }

    /// Returns true if this token carries root/admin privileges.
    pub fn is_admin(&self) -> bool {
        self.scopes.iter().any(|s| s == "*:*:*")
    }

    /// Returns the explicit document keys this token may access for a given
    /// action + collection. Wildcard scopes (`*`) are excluded — use
    /// `has_collection_access` to check those first.
    ///
    /// Used by `handle_get` to scope a query to only the documents the token
    /// is allowed to read when no collection-level wildcard is present.
    pub fn allowed_keys(&self, action: &str, collection: &str) -> Vec<String> {
        self.scopes
            .iter()
            .filter_map(|scope| {
                let parts: Vec<&str> = scope.splitn(3, ':').collect();
                if parts.len() != 3 {
                    return None;
                }
                let (s_action, s_col, s_key) = (parts[0], parts[1], parts[2]);
                let action_match = s_action == action;
                let col_match = s_col == "*" || s_col == collection;
                // Only return concrete (non-wildcard) keys.
                // Prefix patterns (e.g. "store_A_*") are handled by has_access post-filtering.
                if action_match && col_match && !s_key.contains('*') {
                    Some(s_key.to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Extracts the key prefixes this token is allowed to access for a given
    /// action + collection. Only returns prefixes from prefix-wildcard scopes
    /// (e.g. `read:laptops:store_A_*` → `"store_A_"`).
    ///
    /// Returns an empty Vec if the token has full collection access (`*`) or
    /// admin (`*:*:*`) — callers should skip prefix filtering in that case.
    pub fn extract_prefixes(&self, action: &str, collection: &str) -> Vec<String> {
        if self.is_admin() {
            return vec![];
        }
        self.scopes
            .iter()
            .filter_map(|scope| {
                let parts: Vec<&str> = scope.splitn(3, ':').collect();
                if parts.len() != 3 {
                    return None;
                }
                let (s_action, s_col, s_key) = (parts[0], parts[1], parts[2]);
                if s_action != action {
                    return None;
                }
                if s_col != "*" && s_col != collection {
                    return None;
                }
                // Only prefix wildcards (e.g. "store_A_*"), not full wildcard ("*").
                if s_key.ends_with('*') && s_key != "*" {
                    Some(s_key.trim_end_matches('*').to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns true if this token has any prefix-wildcard scope for the given
    /// action + collection (e.g. "read:laptops:store_A_*").
    /// Used by handle_get to decide whether pre-injection of _allowed_prefixes is needed.
    pub fn has_prefix_wildcard(&self, action: &str, collection: &str) -> bool {
        self.scopes.iter().any(|scope| {
            let parts: Vec<&str> = scope.splitn(3, ':').collect();
            if parts.len() != 3 {
                return false;
            }
            let (s_action, s_col, s_key) = (parts[0], parts[1], parts[2]);
            s_action == action
                && (s_col == "*" || s_col == collection)
                && s_key.ends_with('*')
                && s_key != "*"
        })
    }
}

// ─── Scope key pattern matching ──────────────────────────────────────────────

/// Match a scope key pattern against a concrete document key.
///
/// Supported patterns:
///   `"*"`         → matches any key (full wildcard)
///   `"store_A_*"` → matches any key starting with `"store_A_"` (prefix wildcard)
///   `"lp1"`       → exact match only
fn key_matches(pattern: &str, key: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return key.starts_with(prefix);
    }
    pattern == key
}

/// Request body for POST /auth/delegate
#[derive(Debug, Deserialize)]
pub struct DelegateRequest {
    /// A label for the client receiving this token (stored in JWT `sub`).
    pub client_id: String,
    /// List of scopes to embed in the JWT.
    /// e.g. ["read:laptops:lp1", "write:users:usr_123", "read:*:*"]
    pub scopes: Vec<String>,
    /// Optional TTL in seconds. Defaults to 3600 (1 hour).
    pub ttl_secs: Option<u64>,
}

/// Response body for POST /auth/delegate
#[derive(Debug, Serialize)]
pub struct DelegateResponse {
    pub token: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    /// The unique JWT ID embedded in the token.
    /// Use this value as the :jti path parameter when calling DELETE /auth/tokens/:jti.
    pub jti: String,
}

/// The JSON body expected by the POST /login endpoint.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// The JSON body returned by POST /login on success.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    /// The signed JWT token. The client should include this in subsequent
    /// requests as: Authorization: Bearer <token>
    pub token: String,
}

// ─── JWT helpers ──────────────────────────────────────────────────────────────

/// The JWT signing secret, read once at startup from the JWT_SECRET environment
/// variable and cached for the lifetime of the process.
///
/// Using OnceLock avoids acquiring the global OS env-var lock on every request.
/// Falls back to a hardcoded default if the variable is not set.
///
/// WARNING: The default secret is publicly known — anyone can forge tokens
/// signed with it. Always set JWT_SECRET in production.
static JWT_SECRET: OnceLock<String> = OnceLock::new();

fn get_secret() -> &'static str {
    JWT_SECRET.get_or_init(|| {
        std::env::var("JWT_SECRET")
            .unwrap_or_else(|_| "dev-secret-change-in-production".to_string())
    })
}

/// Create a signed JWT token for the given username.
///
/// The token expires 24 hours (86400 seconds) from now.
/// Returns the compact serialization: "header.payload.signature"
pub fn create_token(username: &str) -> Result<String, AuthError> {
    create_scoped_token(username, vec!["*:*:*".to_string()], 86400).map(|(token, _)| token)
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

// ─── Password hashing ─────────────────────────────────────────────────────────
// bcrypt is a deliberately slow password hashing algorithm — it's designed to
// make brute-force attacks expensive even if the hash database is leaked.
// DEFAULT_COST = 12 iterations (takes ~250ms on modern hardware).

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

// ─── HMAC helpers ─────────────────────────────────────────────────────────────

type HmacSha256 = Hmac<Sha256>;

/// Compute an HMAC-SHA256 over `data` using `JWT_SECRET` as the key.
/// Returns the result as a lowercase hex string.
fn hmac_sign(data: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(get_secret().as_bytes()).expect("HMAC accepts any key length");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

/// Verify an HMAC-SHA256 hex tag over `data` using `JWT_SECRET` as the key.
/// Returns `true` if the tag is valid, `false` otherwise.
/// Uses constant-time comparison internally (via the `hmac` crate) to prevent
/// timing attacks.
fn hmac_verify(data: &[u8], expected_hex: &str) -> bool {
    let Ok(expected_bytes) = hex::decode(expected_hex) else {
        return false;
    };
    let mut mac =
        HmacSha256::new_from_slice(get_secret().as_bytes()).expect("HMAC accepts any key length");
    mac.update(data);
    mac.verify_slice(&expected_bytes).is_ok()
}

// ─── Token revocation ────────────────────────────────────────────────────────

/// Newtype wrapper for the revocation store file path.
///
/// Injected as an Axum `Extension` so `handle_revoke` can call
/// `save_to_file` after adding a new entry, without needing to
/// thread the path through the app state tuple.
#[derive(Clone, Debug)]
pub struct RevocationsPath(pub String);

/// In-memory store of revoked JWT IDs (jti).
///
/// When a token is revoked via DELETE /auth/tokens/:jti, its jti is added here.
/// The auth_middleware checks this store on every request and rejects revoked tokens
/// even if they have not yet expired.
///
/// Entries are pruned automatically by a background task every 60 seconds once
/// the token's original expiry has passed (no need to keep them forever).
#[derive(Clone, Default)]
pub struct RevocationStore {
    /// Maps jti → the Instant at which the entry can be safely pruned
    /// (i.e. when the original token would have expired anyway).
    revoked: Arc<DashMap<String, Instant>>,
}

impl RevocationStore {
    /// Create a new, empty revocation store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Revoke a token by its jti. `prune_after` is when the entry can be
    /// discarded (typically the token's own expiry time).
    pub fn revoke(&self, jti: &str, prune_after: Instant) {
        self.revoked.insert(jti.to_string(), prune_after);
    }

    /// Returns true if the given jti has been revoked.
    pub fn is_revoked(&self, jti: &str) -> bool {
        self.revoked.contains_key(jti)
    }

    /// Remove entries whose prune_after time has passed.
    /// Call this from a background task every 60 seconds.
    pub fn prune(&self) {
        self.revoked
            .retain(|_, prune_at| *prune_at > Instant::now());
    }

    /// Persist the current revocation list to a JSON file.
    ///
    /// The file format is:
    ///   `{ "entries": { "<jti>": <prune_unix_secs>, ... }, "sig": "<hmac-sha256-hex>" }`
    ///
    /// The `sig` field is an HMAC-SHA256 of the canonical JSON of `entries`,
    /// keyed with `JWT_SECRET`. On load, the signature is verified before the
    /// entries are trusted — a missing or invalid signature causes the server
    /// to refuse startup (fail-closed).
    ///
    /// This is an async function — it uses `tokio::fs::write` so it does not
    /// block the Tokio worker thread during the disk flush.
    pub async fn save_to_file(&self, path: &str) {
        // Convert Instant → u64 unix seconds for serialization.
        // Instant is not directly serializable, so we compute the remaining
        // duration from now and add it to the current unix timestamp.
        let now_instant = Instant::now();
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let map: std::collections::HashMap<String, u64> = self
            .revoked
            .iter()
            .map(|entry| {
                let jti = entry.key().clone();
                let prune_at = *entry.value();
                // How many seconds remain until prune_at from now?
                let remaining_secs = if prune_at > now_instant {
                    prune_at.duration_since(now_instant).as_secs()
                } else {
                    0
                };
                (jti, now_unix + remaining_secs)
            })
            .collect();

        // Serialize entries to a canonical JSON string — this is what we sign.
        let entries_json = match serde_json::to_string(&map) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("⚠️  Failed to serialize revocation store: {}", e);
                return;
            }
        };

        // Compute HMAC-SHA256 of the entries JSON using JWT_SECRET as the key.
        let sig = hmac_sign(entries_json.as_bytes());

        // Wrap entries + signature into the final file payload.
        let payload = serde_json::json!({
            "entries": map,
            "sig": sig,
        });

        match serde_json::to_string(&payload) {
            Ok(json) => {
                if let Err(e) = tokio::fs::write(path, json).await {
                    eprintln!(
                        "⚠️  Failed to persist revocation store to '{}': {}",
                        path, e
                    );
                }
            }
            Err(e) => eprintln!("⚠️  Failed to serialize revocation store payload: {}", e),
        }
    }

    /// Load the revocation list from a JSON file written by `save_to_file`.
    ///
    /// The file must contain a valid HMAC-SHA256 signature over the `entries`
    /// field (keyed with `JWT_SECRET`). If the file exists but the signature is
    /// missing or invalid, this function returns `Err` — the caller must treat
    /// this as a fatal startup error (fail-closed: do not boot with an untrusted
    /// revocation store).
    ///
    /// A missing file is `Ok(empty store)` — normal on first startup.
    /// Entries whose prune deadline has already passed are silently skipped.
    pub fn load_from_file(path: &str) -> Result<Self, String> {
        let store = Self::default();

        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File missing — fresh start, nothing to load.
                return Ok(store);
            }
            Err(e) => {
                return Err(format!("Failed to read revocation store '{}': {}", path, e));
            }
        };

        // Parse the outer envelope: { "entries": {...}, "sig": "..." }
        let envelope: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(e) => {
                return Err(format!(
                    "Failed to parse revocation store '{}': {}",
                    path, e
                ));
            }
        };

        // Extract and verify the HMAC signature before trusting any entries.
        let sig = match envelope.get("sig").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return Err(format!(
                    "Revocation store '{}' is missing the 'sig' field — \
                     file may have been tampered with or was written by an older version. \
                     Delete the file to start fresh, or restore it from a trusted backup.",
                    path
                ));
            }
        };

        // Re-serialize the entries map to the canonical JSON string that was signed.
        let entries_value = match envelope.get("entries") {
            Some(v) => v,
            None => {
                return Err(format!(
                    "Revocation store '{}' is missing the 'entries' field.",
                    path
                ));
            }
        };
        let entries_json = match serde_json::to_string(entries_value) {
            Ok(j) => j,
            Err(e) => {
                return Err(format!(
                    "Failed to re-serialize entries from '{}': {}",
                    path, e
                ));
            }
        };

        // Verify the HMAC — fail-closed if it doesn't match.
        if !hmac_verify(entries_json.as_bytes(), &sig) {
            return Err(format!(
                "Revocation store '{}' has an invalid HMAC signature — \
                 the file may have been tampered with. \
                 Delete the file to start fresh, or restore it from a trusted backup.",
                path
            ));
        }

        // Signature is valid — deserialize the entries map.
        let map: std::collections::HashMap<String, u64> =
            match serde_json::from_value(entries_value.clone()) {
                Ok(m) => m,
                Err(e) => {
                    return Err(format!(
                        "Failed to deserialize revocation entries from '{}': {}",
                        path, e
                    ));
                }
            };

        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let now_instant = Instant::now();

        for (jti, prune_unix) in map {
            if prune_unix <= now_unix {
                // Already expired — skip it.
                continue;
            }
            let remaining = std::time::Duration::from_secs(prune_unix - now_unix);
            let prune_instant = now_instant + remaining;
            store.revoked.insert(jti, prune_instant);
        }

        Ok(store)
    }
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

// ─── Auth error type ──────────────────────────────────────────────────────────

/// Unified error type for all public `moltendb-auth` API functions.
#[derive(Debug)]
pub enum AuthError {
    /// The token failed signature validation or is otherwise malformed.
    InvalidToken(jsonwebtoken::errors::Error),
    /// The token's `exp` claim is in the past — it has expired.
    TokenExpired,
    /// The token has been explicitly revoked via the revocation store.
    TokenRevoked,
    /// Attempted to refresh a root (`*:*:*`) token — not allowed.
    RefreshNotAllowed,
    /// A role name was referenced that does not exist in the `RoleStore`.
    RoleNotFound(String),
    /// A bcrypt password hashing or verification operation failed.
    HashError(bcrypt::BcryptError),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::InvalidToken(e) => write!(f, "Invalid token: {}", e),
            AuthError::TokenExpired => write!(f, "Token has expired"),
            AuthError::TokenRevoked => write!(f, "Token has been revoked"),
            AuthError::RefreshNotAllowed => {
                write!(f, "Root tokens cannot be refreshed via this endpoint")
            }
            AuthError::RoleNotFound(role) => write!(f, "Role not found: {}", role),
            AuthError::HashError(e) => write!(f, "Password hashing error: {}", e),
        }
    }
}

// ─── Auth middleware ──────────────────────────────────────────────────────────

/// Axum middleware that enforces JWT authentication on protected routes.
///
/// This function is registered as a layer on the protected_routes router in
/// main.rs. It runs before every request to a protected endpoint.
///
/// Flow:
///   1. Read the Authorization header.
///   2. Extract the Bearer token.
///   3. Verify the token's signature and expiry.
///   4. Check the token's jti against the RevocationStore.
///   5. If valid and not revoked: attach the Claims to the request and call next.run(request).
///   6. If invalid or revoked: return 401 Unauthorized immediately.
pub async fn auth_middleware(
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, impl IntoResponse> {
    // Read the Authorization header value as a string.
    let auth_header = headers.get("Authorization").and_then(|h| h.to_str().ok());

    // Extract the token from "Bearer <token>".
    // The [7..] slice skips the "Bearer " prefix (7 characters).
    let token = match auth_header {
        Some(header) if header.starts_with("Bearer ") => &header[7..],
        _ => {
            // Missing or malformed Authorization header — reject immediately.
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Missing or invalid Authorization header"})),
            ));
        }
    };

    // Verify the token signature and expiry.
    let claims = match verify_token(token) {
        Ok(c) => c,
        Err(_) => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Invalid or expired token"})),
            ));
        }
    };

    // Check the revocation store — reject tokens that have been explicitly revoked.
    // The RevocationStore is always injected as an Extension by main.rs.
    // If it is somehow missing, fail closed (reject the request) rather than
    // silently skipping the revocation check.
    match request.extensions().get::<RevocationStore>() {
        Some(store) => {
            // Fail-closed: reject tokens that have no jti — they cannot be checked
            // against the revocation store and may be legacy or crafted tokens.
            if claims.jti.is_empty() {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Token missing required jti claim"})),
                ));
            }
            if store.is_revoked(&claims.jti) {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Token has been revoked"})),
                ));
            }
        }
        None => {
            // RevocationStore not found in extensions — this is a server
            // misconfiguration. Fail closed to avoid bypassing revocation.
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Internal error: revocation store unavailable"})),
            ));
        }
    }

    // Attach the Claims to the request so downstream handlers can read them via:
    //   Extension(claims): Extension<Claims>
    request.extensions_mut().insert(claims);
    // Pass the request to the next handler/middleware in the chain.
    Ok(next.run(request).await)
}

// ─── User store ───────────────────────────────────────────────────────────────

// DashMap = concurrent hash map — safe to read/write from multiple threads.
use dashmap::DashMap;
// Arc = thread-safe reference-counted pointer for shared ownership.
use std::sync::Arc;

/// In-memory user store holding the single admin user's bcrypt password hash.
///
/// MoltenDB v1 supports exactly one user (the admin). The username and password
/// are loaded from environment variables at startup. There is no user management
/// API — adding or removing users requires a server restart with updated credentials.
#[derive(Clone)]
pub struct UserStore {
    /// Maps username → bcrypt hash of the password.
    /// Arc allows UserStore to be cheaply cloned and shared across Axum handlers.
    users: Arc<DashMap<String, String>>,
}

impl UserStore {
    /// Create a new UserStore and populate it with the admin user.
    ///
    /// The admin username and password are provided as arguments.
    /// The password is hashed with bcrypt before storing — the plaintext is
    /// never kept in memory after this function returns.
    ///
    /// Returns `Err` if bcrypt fails to hash the password (e.g. RNG exhaustion).
    /// The caller should treat this as a fatal startup error — a store with no
    /// users would permanently lock out the admin with no indication.
    pub fn new(username: String, password: String) -> Result<Self, AuthError> {
        let store = Self {
            users: Arc::new(DashMap::new()),
        };

        // Hash the password and store the hash (never the plaintext).
        // Propagate the error — a failed hash means zero users in the store,
        // which would silently lock out the admin on startup.
        let hash = hash_password(&password)?;
        store.users.insert(username, hash);

        Ok(store)
    }

    /// Verify a username + password pair against the stored hash.
    ///
    /// Returns true if the username exists and the password matches its hash.
    /// Returns false if the username doesn't exist or the password is wrong.
    /// bcrypt::verify() is timing-safe — it takes the same time regardless of
    /// whether the password is correct, preventing timing attacks.
    pub fn verify_user(&self, username: &str, password: &str) -> bool {
        if let Some(hash) = self.users.get(username) {
            // verify_password() returns Ok(true/false) or Err on internal error.
            // unwrap_or(false) treats internal errors as "password incorrect".
            verify_password(password, hash.value()).unwrap_or(false)
        } else {
            false // Username not found
        }
    }
}
