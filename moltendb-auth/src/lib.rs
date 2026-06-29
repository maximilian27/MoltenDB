#![cfg(not(target_arch = "wasm32"))]
#![deny(warnings)]
// ─── moltendb-auth ────────────────────────────────────────────────────────────
// Authentication and authorisation for the MoltenDB server.
//
// Modules:
//   types      — Claims, request/response structs, AuthError
//   hmac       — HMAC-SHA256 helpers and scope key pattern matching
//   token      — JWT creation, verification, and token refresh
//   password   — bcrypt password hashing and verification
//   store      — RevocationStore, UserStore, RevocationsPath
//   middleware — Axum auth_middleware layer
// ─────────────────────────────────────────────────────────────────────────────

// moltendb-auth is a native-only crate — it is never compiled for WASM targets.

pub mod hmac;
pub mod middleware;
pub mod password;
pub mod store;
pub mod token;
pub mod types;

// Re-export the public API so callers can use `moltendb_auth::Claims` etc.
// without needing to know which submodule each item lives in.
pub use middleware::auth_middleware;
pub use password::{hash_password, verify_password};
pub use store::{RevocationStore, RevocationsPath, UserStore};
pub use token::{create_scoped_token, create_token, refresh_scoped_token, verify_token};
pub use types::{
    AuthError, Claims, DelegateRequest, DelegateResponse, LoginRequest, LoginResponse,
};

#[cfg(test)]
mod tests;
