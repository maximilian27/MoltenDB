<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-auth

### 🔐 The Identity Crate

**JWT minting & validation · Scoped token delegation · Argon2 password hashing · Axum auth middleware**  
No knowledge of HTTP routing, TLS, or the database engine.

[![License](https://img.shields.io/badge/license-Elastic--2.0-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![crates.io](https://img.shields.io/crates/v/moltendb-auth?style=flat-square)](https://crates.io/crates/moltendb-auth)
[![Status](https://img.shields.io/badge/status-1.0.0--rc-blue?style=flat-square)](../CHANGELOG.md)

</div>

> [!WARNING]
> **After careful consideration, a breaking change was introduced in v1.0.0-rc10. Versions starting with `v1.0.0-rc10`
are not backwards compatible with previous versions.**
> Review the [changelog](../CHANGELOG.md) before upgrading.

---

## What is this crate?

`moltendb-auth` is the identity layer of MoltenDB. It handles everything related to authentication and authorisation and
is consumed exclusively by `moltendb-server`. It has no knowledge of the database engine, HTTP routing, or TLS.

> **WASM note:** This entire crate is excluded from WASM builds via `#![cfg(not(target_arch = "wasm32"))]`. Auth is
> irrelevant for local browser storage and would add unnecessary weight to the WASM bundle.

- **Argon2 password hashing** — passwords are hashed with Argon2id; plain-text passwords never leave this crate.
- **JWT minting & validation** — tokens are signed with HMAC-SHA256 (`jsonwebtoken`). Each token carries a `sub` (
  username), `exp` (expiry), and `scopes` (permission list).
- **Scoped token delegation** — the root user can mint narrow-permission JWTs for clients via `create_scoped_token`.
  Clients only ever receive a token scoped to exactly what they need.
- **Token revocation (JTI blacklist)** — every JWT carries a `jti` (UUID). Compromised tokens can be immediately
  invalidated via `DELETE /auth/tokens/:jti` before their TTL expires. The revocation store is persisted to disk and
  survives server restarts.
- **`UserStore`** — an in-memory `DashMap` mapping usernames to Argon2 hashes. Seeded at startup from CLI args (
  `--root-user` / `--root-password`).
- **Axum `auth_middleware`** — a `tower` middleware layer that extracts the `Authorization: Bearer <token>` header,
  validates the JWT, and rejects unauthenticated requests with `401 Unauthorized`.

---

## Scope Format

Scopes are strings embedded in the JWT payload. Every protected endpoint checks the token's scopes before processing the
request.

```
action:collection:document_key
```

| Scope                 | Meaning                                                   |
|-----------------------|-----------------------------------------------------------|
| `read:laptops:lp1`    | Read only document `lp1` in the `laptops` collection      |
| `write:users:usr_123` | Write only document `usr_123` in the `users` collection   |
| `read:laptops:*`      | Read any document in the `laptops` collection             |
| `write:laptops:*`     | Write any document in the `laptops` collection            |
| `delete:laptops:*`    | Delete any document in the `laptops` collection           |
| `read:*:*`            | Read any document in any collection                       |
| `*:*:*`               | Full admin access — read, write, delete across everything |

The root user's token always carries `*:*:*`. Only the root user can mint new `*:*:*` tokens. The root token TTL
defaults to 86400 seconds (24 hours) and is configurable via `--root-token-ttl` / `MOLTENDB_ROOT_TOKEN_TTL`.

---

## Scope → Endpoint Compatibility

| Token scope        | `POST /get`       | `GET /collections/:col/docs/:key` | `POST /set` | `POST /update` | `POST /delete` | `POST /snapshot` |
|--------------------|-------------------|-----------------------------------|-------------|----------------|----------------|------------------|
| `read:laptops:lp1` | filtered to lp1   | ✅ lp1 only                        | ❌ 403       | ❌ 403          | ❌ 403          | ❌ 403            |
| `read:laptops:*`   | ✅ full collection | ✅ any key                         | ❌ 403       | ❌ 403          | ❌ 403          | ❌ 403            |
| `write:laptops:*`  | ❌ 403             | ❌ 403                             | ✅           | ✅              | ❌ 403          | ❌ 403            |
| `delete:laptops:*` | ❌ 403             | ❌ 403                             | ❌ 403       | ❌ 403          | ✅              | ❌ 403            |
| `read:*:*`         | ✅ any collection  | ✅ any col/key                     | ❌ 403       | ❌ 403          | ❌ 403          | ❌ 403            |
| `*:*:*`            | ✅                 | ✅                                 | ✅           | ✅              | ✅              | ✅                |

---

## Public API

```rust
// Seed the store with the root user at startup.
// Returns Err if bcrypt fails — treat as fatal (abort startup).
let store = UserStore::new("root".into(), "my-secret-password".into())
.expect("Failed to hash admin password during startup");

// Mint a root JWT (carries *:*:* scope, TTL controlled by MOLTENDB_ROOT_TOKEN_TTL, default 86400s)
let token = moltendb_auth::create_token("root", 86400) ?;

// Mint a scoped JWT for a client (custom scopes + TTL)
// Returns (token, jti) — store the jti if you need to revoke this token later
let (token, jti) = moltendb_auth::create_scoped_token(
"laptop-service",
vec!["read:laptops:*".to_string(), "write:laptops:*".to_string()],
3600, // TTL in seconds
) ?;

// Verify a JWT and extract claims (called inside auth_middleware)
let claims = moltendb_auth::verify_token( & token) ?;

// Check if a token grants access to a specific action + collection + document
if claims.has_access("read", "laptops", "lp1") { /* allowed */ }

// Check collection-level access (key wildcard)
if claims.has_collection_access("read", "laptops") { /* allowed */ }

// Check if the token is a full admin token
if claims.is_admin() { /* *:*:* scope present */ }

// Get the list of document keys a token may access for a given action + collection
let keys: Vec<String> = claims.allowed_keys("read", "laptops");
// → ["lp1", "lp2"] for a token with read:laptops:lp1 and read:laptops:lp2

// Revoke a token by its jti (blocks it immediately, before TTL expires)
revocation_store.revoke( & jti, std::time::Instant::now() + std::time::Duration::from_secs(ttl));

// Check if a jti has been revoked (called automatically inside auth_middleware)
if revocation_store.is_revoked( & jti) { /* reject */ }

// Load the revocation store from disk at startup.
// Returns Err if the file exists but has a missing or invalid HMAC-SHA256 signature
// (tamper-evident, fail-closed). A missing file returns Ok(empty store).
let store = RevocationStore::load_from_file("my_database.revocations.json")
.expect("Revocation store integrity check failed — possible tampering");

// Persist the revocation store to disk (async, signs with JWT_SECRET).
// File format: { "entries": { "<jti>": <prune_unix_secs> }, "sig": "<hmac-sha256-hex>" }
revocation_store.save_to_file("my_database.revocations.json").await;

// Refresh a scoped token — returns a new (token, jti) with the same sub + scopes.
// Root tokens (*:*:*) return Err(AuthError::RefreshNotAllowed).
// The old jti is immediately revoked in the store; persist afterwards.
let (new_token, new_jti) = moltendb_auth::refresh_scoped_token(
& old_token,
3600, // new TTL in seconds
& revocation_store,
) ?;
revocation_store.save_to_file("my_database.revocations.json").await;

// Hash a password (Argon2id)
let hash = moltendb_auth::hash_password("my-secret-password") ?;

// Verify a password against its hash
let ok = moltendb_auth::verify_password("my-secret-password", & hash) ?;
```

### Axum middleware

```rust
use moltendb_auth::auth_middleware;
use axum::middleware;

let protected = Router::new()
.route("/set", post(handle_set))
.route("/get", post(handle_get))
// ... other routes
.layer(middleware::from_fn(auth_middleware));
```

---

## Types

| Type               | Description                                                                                                                                                                                                            |
|--------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `UserStore`        | In-memory `DashMap<String, String>` — username → Argon2 hash                                                                                                                                                           |
| `Claims`           | JWT payload: `sub` (username), `exp` (Unix timestamp), `scopes: Vec<String>`                                                                                                                                           |
| `LoginRequest`     | `{ username: String, password: String }`                                                                                                                                                                               |
| `LoginResponse`    | `{ token: String }`                                                                                                                                                                                                    |
| `DelegateRequest`  | `{ client_id: String, scopes: Vec<String>, ttl_secs: Option<u64> }`                                                                                                                                                    |
| `DelegateResponse` | `{ token: String, jti: String, client_id: String, scopes: Vec<String> }` — `jti` is the UUID to use for revocation                                                                                                     |
| `AuthError`        | `InvalidToken(jsonwebtoken::errors::Error)` \| `TokenExpired` \| `TokenRevoked` \| `RefreshNotAllowed` \| `RoleNotFound(String)` \| `HashError(bcrypt::BcryptError)` — unified error type for all public API functions |
| `RevocationStore`  | In-memory `DashMap<String, Instant>` — revoked JTIs with their prune deadline. Persisted as `{ "entries": {...}, "sig": "<hmac-sha256-hex>" }`; signature verified on load (fail-closed).                              |

---

## Integration Pattern — Bring Your Own User Table

MoltenDB is designed to work alongside your existing auth system. MoltenDB never stores your users — it only knows about
the root user. Your backend validates credentials against your own database, then calls `POST /auth/delegate` to mint a
scoped MoltenDB token for the client.

```
Your App Backend                          MoltenDB
─────────────────                         ────────
1. User logs in → validate against        
   your PostgreSQL / MySQL / etc.         
                                          
2. POST /auth/delegate                →   Validates root token
   Authorization: Bearer <root-token>     Mints scoped JWT
   { client_id, scopes, ttl_secs }    ←   Returns { token }
                                          
3. Return scoped token to client          
                                          
4. Client uses scoped token directly  →   Enforces scopes on every request
   GET /collections/laptops/docs/lp1      Returns 403 if scope missing
```

The root token never leaves your backend. Clients only ever receive a narrowly scoped token.

---

## Current limitations (v1.0.0-rc)

- **No token refresh** — tokens expire after the configured TTL. Re-mint via `/auth/delegate` when needed.
- **In-memory revocation only** — the revocation store is persisted to a `.revocations.json` file alongside the WAL and
  reloaded on startup, but revocations are not replicated across nodes.
- **JWT secret via CLI arg** — `--jwt-secret` appears in the process list. For production, pass it via the
  `MOLTENDB_JWT_SECRET` environment variable instead.

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
