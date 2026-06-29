// ─── JWT data structures ──────────────────────────────────────────────────────

use serde::{Deserialize, Serialize};

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
            let key_match = crate::hmac::key_matches(s_key, doc_key);
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
                write!(
                    f,
                    "Admin tokens (*:*:*) cannot be refreshed via this endpoint — re-delegate via POST /auth/delegate"
                )
            }
            AuthError::RoleNotFound(role) => write!(f, "Role not found: {}", role),
            AuthError::HashError(e) => write!(f, "Password hashing error: {}", e),
        }
    }
}
