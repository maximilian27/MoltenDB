// ─── Auth middleware ──────────────────────────────────────────────────────────

use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::store::RevocationStore;
use crate::token::verify_token;

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
