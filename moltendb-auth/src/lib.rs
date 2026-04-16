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
//     MOLTENDB_ADMIN_USER   
//     MOLTENDB_ADMIN_PASSWORD 
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
    http::{StatusCode, HeaderMap},
    middleware::Next,
    response::{Response, IntoResponse},
    Json,
};
// JWT encoding/decoding functions and types.
use jsonwebtoken::{encode, decode, Header, Validation, EncodingKey, DecodingKey};
// Serde traits for serializing/deserializing the Claims struct to/from JSON.
use serde::{Deserialize, Serialize};
use serde_json::json;
// SystemTime and UNIX_EPOCH for computing token expiry timestamps.
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Read the JWT signing secret from the JWT_SECRET environment variable.
/// Falls back to a hardcoded default if the variable is not set.
///
/// WARNING: The default secret is publicly known — anyone can forge tokens
/// signed with it. Always set JWT_SECRET in production.
fn get_secret() -> String {
    std::env::var("JWT_SECRET").unwrap_or_else(|_| "dev-secret-change-in-production".to_string())
}

/// Create a signed JWT token for the given username.
///
/// The token expires 24 hours (86400 seconds) from now.
/// Returns the compact serialization: "header.payload.signature"
pub fn create_token(username: &str) -> Result<String, jsonwebtoken::errors::Error> {
    // Compute the expiry timestamp: current Unix time + 24 hours.
    let expiration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() + 86400; // 86400 seconds = 24 hours

    let claims = Claims {
        sub: username.to_string(),
        exp: expiration,
    };

    // encode() signs the claims with HMAC-SHA256 using the secret key.
    // Header::default() uses the HS256 algorithm.
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(get_secret().as_bytes()),
    )
}

/// Verify a JWT token and return the decoded Claims if valid.
///
/// This checks:
///   1. The signature — was this token signed with our secret?
///   2. The expiry — has the token expired?
///
/// Returns Err if the token is invalid, expired, or malformed.
pub fn verify_token(token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(get_secret().as_bytes()),
        // Validation::default() checks signature + expiry automatically.
        &Validation::default(),
    )?;
    Ok(token_data.claims)
}

// ─── Password hashing ─────────────────────────────────────────────────────────
// bcrypt is a deliberately slow password hashing algorithm — it's designed to
// make brute-force attacks expensive even if the hash database is leaked.
// DEFAULT_COST = 12 iterations (takes ~250ms on modern hardware).

/// Hash a plaintext password using bcrypt. Returns the hash string.
/// Store the hash, never the plaintext password.
pub fn hash_password(password: &str) -> Result<String, bcrypt::BcryptError> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST)
}

/// Verify a plaintext password against a stored bcrypt hash.
/// Returns true if the password matches, false otherwise.
pub fn verify_password(password: &str, hash: &str) -> Result<bool, bcrypt::BcryptError> {
    bcrypt::verify(password, hash)
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
///   4. If valid: attach the Claims to the request and call next.run(request).
///   5. If invalid: return 401 Unauthorized immediately.
pub async fn auth_middleware(
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, impl IntoResponse> {
    // Read the Authorization header value as a string.
    let auth_header = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok());

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

    // Verify the token. If valid, attach the Claims to the request so
    // downstream handlers can read the authenticated username via:
    //   request.extensions().get::<Claims>()
    match verify_token(token) {
        Ok(claims) => {
            request.extensions_mut().insert(claims);
            // Pass the request to the next handler/middleware in the chain.
            Ok(next.run(request).await)
        }
        Err(_) => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid or expired token"})),
        )),
    }
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
    pub fn new(username: String, password: String) -> Self {
        let store = Self {
            users: Arc::new(DashMap::new()),
        };

        // Hash the password and store the hash (never the plaintext).
        if let Ok(hash) = hash_password(&password) {
            store.users.insert(username, hash);
        }

        store
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
