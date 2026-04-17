<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-auth

### 🔐 The Identity Crate

**JWT minting & validation · Argon2 password hashing · Axum auth middleware**  
No knowledge of HTTP routing, TLS, or the database engine.

[![License](https://img.shields.io/badge/license-BSL%201.1-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![crates.io](https://img.shields.io/crates/v/moltendb-auth?style=flat-square)](https://crates.io/crates/moltendb-auth)

</div>

---

## What is this crate?

`moltendb-auth` is the identity layer of MoltenDB. It handles everything related to authentication and is consumed exclusively by `moltendb-server`. It has no knowledge of the database engine, HTTP routing, or TLS.

- **Argon2 password hashing** — passwords are hashed with Argon2id before storage; plain-text passwords never leave this crate.
- **JWT minting & validation** — tokens are signed with HMAC-SHA256 (`jsonwebtoken`). Each token carries a `sub` (username) and `exp` (expiry, 24 h).
- **`UserStore`** — an in-memory `DashMap` mapping usernames to Argon2 hashes. Seeded at startup from CLI args (`--admin-user` / `--admin-password`).
- **Axum `auth_middleware`** — a `tower` middleware layer that extracts the `Authorization: Bearer <token>` header, validates the JWT, and rejects unauthenticated requests with `401 Unauthorized`.

---

## Public API

```rust
// Seed the store with the admin user at startup
let store = UserStore::new("admin".into(), "my-secret-password".into());

// Mint a JWT for a verified user (called from the /login handler)
let token = moltendb_auth::create_token("admin", &jwt_secret)?;

// Verify a JWT (called inside auth_middleware)
let claims = moltendb_auth::verify_token(&token, &jwt_secret)?;

// Hash a password (Argon2id)
let hash = moltendb_auth::hash_password("my-secret-password")?;

// Verify a password against its hash
let ok = moltendb_auth::verify_password("my-secret-password", &hash)?;
```

### Axum middleware

```rust
use moltendb_auth::auth_middleware;
use axum::middleware;

let protected = Router::new()
    .route("/set",    post(handle_set))
    .route("/get",    post(handle_get))
    // ... other routes
    .layer(middleware::from_fn(auth_middleware));
```

---

## Types

| Type | Description |
|---|---|
| `UserStore` | In-memory `DashMap<String, String>` — username → Argon2 hash |
| `Claims` | JWT payload: `sub` (username), `exp` (Unix timestamp) |
| `LoginRequest` | `{ username: String, password: String }` |
| `LoginResponse` | `{ token: String }` |

---

## Current limitations (v0.3.x)

- **Single-user only** — one admin user is configured at startup via `--admin-user` / `--admin-password`. There is no HTTP endpoint to create or delete users at runtime.
- **No token refresh** — tokens expire after 24 hours. Re-login is required.
- **No token revocation** — once issued, a JWT is valid until expiry. There is no blacklist or session invalidation mechanism.
- **JWT secret via CLI arg** — `--jwt-secret` appears in the process list. For production, pass it via the `JWT_SECRET` environment variable instead.

---

## Making auth optional

`moltendb-server` compiles `moltendb-auth` behind the `auth` Cargo feature (enabled by default). To bring your own auth layer:

```toml
# Cargo.toml
moltendb-server = { version = "0.3.0-beta.2", default-features = false }
```

Then wrap the Axum router with your own `tower::Layer` before calling `serve()`. See the [server README](../moltendb-server/README.md) for details.

---

## Part of the MoltenDB workspace

```
MoltenDB/
├── moltendb-core/     — pure engine (DashMap, WAL, query evaluator)
├── moltendb-wasm/     — browser adapter (wasm-bindgen glue, WorkerDb, OPFS)
├── moltendb-auth/     ← you are here
└── moltendb-server/   — network layer (Axum, TLS, CORS, CLI config)
```

See the [root README](../README.md) for the full architecture overview and feature list.
