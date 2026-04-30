<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-auth

### 🔐 The Identity Crate

**JWT minting & validation · Scoped token delegation · Argon2 password hashing · Axum auth middleware**  
No knowledge of HTTP routing, TLS, or the database engine.

[![License](https://img.shields.io/badge/license-BSL%201.1-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![crates.io](https://img.shields.io/crates/v/moltendb-auth?style=flat-square)](https://crates.io/crates/moltendb-auth)

</div>

---

## What is this crate?

`moltendb-auth` is the identity layer of MoltenDB. It handles everything related to authentication and authorisation and is consumed exclusively by `moltendb-server`. It has no knowledge of the database engine, HTTP routing, or TLS.

> **WASM note:** This entire crate is excluded from WASM builds via `#![cfg(not(target_arch = "wasm32"))]`. Auth is irrelevant for local browser storage and would add unnecessary weight to the WASM bundle.

- **Argon2 password hashing** — passwords are hashed with Argon2id; plain-text passwords never leave this crate.
- **JWT minting & validation** — tokens are signed with HMAC-SHA256 (`jsonwebtoken`). Each token carries a `sub` (username), `exp` (expiry), and `scopes` (permission list).
- **Scoped token delegation** — the root user can mint narrow-permission JWTs for clients via `create_scoped_token`. Clients only ever receive a token scoped to exactly what they need.
- **`UserStore`** — an in-memory `DashMap` mapping usernames to Argon2 hashes. Seeded at startup from CLI args (`--root-user` / `--root-password`).
- **Axum `auth_middleware`** — a `tower` middleware layer that extracts the `Authorization: Bearer <token>` header, validates the JWT, and rejects unauthenticated requests with `401 Unauthorized`.

---

## Scope Format

Scopes are strings embedded in the JWT payload. Every protected endpoint checks the token's scopes before processing the request.

```
action:collection:document_key
```

| Scope | Meaning |
|---|---|
| `read:laptops:lp1` | Read only document `lp1` in the `laptops` collection |
| `write:users:usr_123` | Write only document `usr_123` in the `users` collection |
| `read:laptops:*` | Read any document in the `laptops` collection |
| `write:laptops:*` | Write any document in the `laptops` collection |
| `delete:laptops:*` | Delete any document in the `laptops` collection |
| `read:*:*` | Read any document in any collection |
| `*:*:*` | Full admin access — read, write, delete across everything |

The root user's token always carries `*:*:*`. Only the root user can mint new `*:*:*` tokens.

---

## Scope → Endpoint Compatibility

| Token scope | `POST /get` | `GET /collections/:col/docs/:key` | `POST /set` | `POST /update` | `POST /delete` | `POST /snapshot` |
|---|---|---|---|---|---|---|
| `read:laptops:lp1` | filtered to lp1 | ✅ lp1 only | ❌ 403 | ❌ 403 | ❌ 403 | ❌ 403 |
| `read:laptops:*` | ✅ full collection | ✅ any key | ❌ 403 | ❌ 403 | ❌ 403 | ❌ 403 |
| `write:laptops:*` | ❌ 403 | ❌ 403 | ✅ | ✅ | ❌ 403 | ❌ 403 |
| `delete:laptops:*` | ❌ 403 | ❌ 403 | ❌ 403 | ❌ 403 | ✅ | ❌ 403 |
| `read:*:*` | ✅ any collection | ✅ any col/key | ❌ 403 | ❌ 403 | ❌ 403 | ❌ 403 |
| `*:*:*` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

---

## Public API

```rust
// Seed the store with the root user at startup
let store = UserStore::new("root".into(), "my-secret-password".into());

// Mint a root JWT (carries *:*:* scope, 24-hour TTL)
let token = moltendb_auth::create_token("root")?;

// Mint a scoped JWT for a client (custom scopes + TTL)
let token = moltendb_auth::create_scoped_token(
    "laptop-service",
    vec!["read:laptops:*".to_string(), "write:laptops:*".to_string()],
    3600, // TTL in seconds
)?;

// Verify a JWT and extract claims (called inside auth_middleware)
let claims = moltendb_auth::verify_token(&token)?;

// Check if a token grants access to a specific action + collection + document
if claims.has_access("read", "laptops", "lp1") { /* allowed */ }

// Check collection-level access (key wildcard)
if claims.has_collection_access("read", "laptops") { /* allowed */ }

// Check if the token is a full admin token
if claims.is_admin() { /* *:*:* scope present */ }

// Get the list of document keys a token may access for a given action + collection
let keys: Vec<String> = claims.allowed_keys("read", "laptops");
// → ["lp1", "lp2"] for a token with read:laptops:lp1 and read:laptops:lp2

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
| `Claims` | JWT payload: `sub` (username), `exp` (Unix timestamp), `scopes: Vec<String>` |
| `LoginRequest` | `{ username: String, password: String }` |
| `LoginResponse` | `{ token: String }` |
| `DelegateRequest` | `{ client_id: String, scopes: Vec<String>, ttl_secs: Option<u64> }` |
| `DelegateResponse` | `{ token: String, client_id: String, scopes: Vec<String> }` |

---

## Integration Pattern — Bring Your Own User Table

MoltenDB is designed to work alongside your existing auth system. MoltenDB never stores your users — it only knows about the root user. Your backend validates credentials against your own database, then calls `POST /auth/delegate` to mint a scoped MoltenDB token for the client.

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

## Current limitations (v0.8.0)

- **Single root user** — one root user is configured at startup via `--root-user` / `--root-password`. There is no HTTP endpoint to create or delete users at runtime. Your own user table handles that.
- **No token refresh** — tokens expire after the configured TTL. Re-mint via `/auth/delegate` when needed.
- **No token revocation** — once issued, a JWT is valid until expiry. There is no blacklist or session invalidation mechanism. Use short TTLs for sensitive scopes.
- **JWT secret via CLI arg** — `--jwt-secret` appears in the process list. For production, pass it via the `MOLTENDB_JWT_SECRET` environment variable instead.

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
