<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-server

### 🚀 The Network Layer Crate

**Axum HTTP server · TLS · Scoped JWT auth · Token delegation · CORS · Rate limiting · WebSocket · CLI config**  
The runnable binary. Delegates all database logic to `moltendb-core`.

[![License](https://img.shields.io/badge/license-Elastic--2.0-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-59%20passing-brightgreen?style=flat-square)](#testing)
[![Status](https://img.shields.io/badge/status-1.0.0--rc-blue?style=flat-square)](../CHANGELOG.md)

</div>

---

## What is this crate?

`moltendb-server` is the runnable binary of MoltenDB. It owns everything related to the network layer and leaves all
database logic to `moltendb-core`:

- **Axum HTTP server** with TLS termination (`axum-server` + `rustls`).
- **Unified Configuration** — Owns the CLI flags and environment variable parsing logic. It populates the core engine's
  `DbConfig` from these inputs.
- **REST endpoints** — `POST /set`, `POST /get`, `POST /update`, `POST /delete`, `POST /snapshot`, `POST /analytics` (⚠️
  analytics under development).
- **REST-style GET** — `GET /collections/:collection/docs/:key` and `GET /collections/:collection` (paginated).
- **Token delegation** — `POST /auth/delegate` (root-only) mints narrow-scoped JWTs for clients; every endpoint enforces
  scopes on every request.
- **WebSocket** — `GET /ws` for real-time mutation notifications.
- **CLI Utility** — `moltendb recover` subcommand for Point-in-Time Recovery.
- **JWT auth middleware** — all protected routes require a valid `Authorization: Bearer <token>` header; scopes are
  checked per-endpoint.
- **Per-IP rate limiting** — sliding-window rate limiter, configurable via CLI.
- **CORS** — configurable allowed origins.
- **Request body size limit** — oversized requests are rejected at the HTTP layer before application code sees them.
- **Graceful shutdown** — drains in-flight requests and flushes the WAL before exit.
- **CLI configuration** — all options are configurable via `clap` flags or environment variables.

---

## Quick start

### Install from source

```bash
cargo install --path moltendb-server
```

### Run

```bash
moltendb \
  --db-path my_database.log \
  --write-mode async \
  --jwt-secret my-awesome-secret \
  --root-user admin \
  --root-password admin123 \
  --cors-origin "*"
```

Or with `cargo run` from the workspace root:

```bash
cargo run --package moltendb-server --bin moltendb -- \
  --db-path my_database.log \
  --write-mode async \
  --jwt-secret my-awesome-secret \
  --root-user admin \
  --root-password admin123 \
  --cors-origin "*"
```

---

## CLI reference

All options can be set via CLI flags or environment variables. CLI flags take priority.

> **Note:** Networking and authentication flags are exclusive to the server binary. If you use `moltendb-core` as a
> library, you must configure it programmatically via `DbConfig`.

| Flag                    | Env var                       | Default           | Description                                             |
|-------------------------|-------------------------------|-------------------|---------------------------------------------------------|
| `--cert`                | `MOLTENDB_TLS_CERT`           | `cert.pem`        | TLS certificate PEM file                                |
| `--cors-origin`         | `MOLTENDB_CORS_ORIGIN`        | `*`               | Allowed CORS origin(s), comma-separated                 |
| `--db-path`             | `MOLTENDB_DB_PATH`            | `my_database.log` | Path to the WAL log file                                |
| `--debug`               | `MOLTENDB_DEBUG`              | `false`           | Enable verbose debug logging                            |
| `--disable-encryption`  | `MOLTENDB_DISABLE_ENCRYPTION` | `false`           | Disable at-rest encryption                              |
| `--encryption-key`      | `MOLTENDB_ENCRYPTION_KEY`     | —                 | At-rest encryption password (ChaCha20-Poly1305)         |
| `--jwt-secret`          | `MOLTENDB_JWT_SECRET`         | **required**      | JWT signing secret — server refuses to start without it |
| `--key`                 | `MOLTENDB_TLS_KEY`            | `key.pem`         | TLS private key PEM file                                |
| `--max-body-size`       | `MOLTENDB_MAX_BODY_SIZE`      | `10485760`        | Max request body size in bytes (default 10 MB)          |
| `--port`                | `MOLTENDB_PORT`               | `1538`            | Port to listen on                                       |
| `--rate-limit-requests` | `MOLTENDB_RATE_LIMIT_REQS`    | `100`             | Max requests per IP per window                          |
| `--rate-limit-window`   | `MOLTENDB_RATE_LIMIT_WINDOW`  | `60`              | Rate limit sliding window in seconds                    |
| `--root-password`       | `MOLTENDB_ROOT_PASSWORD`      | —                 | Root password seeded at startup                         |
| `--root-user`           | `MOLTENDB_ROOT_USER`          | —                 | Root username seeded at startup                         |
| `--write-mode`          | `MOLTENDB_WRITE_MODE`         | `async`           | `async` or `sync`                                       |

### Point-in-Time Recovery (PITR)

MoltenDB supports recovering the database to any millisecond or sequence number.

#### Via CLI

```bash
# Recover to a specific Unix timestamp (milliseconds)
moltendb recover --log my_database.log --to-time 1713972000000 --out recovered.snapshot.bin

# Recover to a specific log sequence number
moltendb recover --log my_database.log --to-seq 5000 --out recovered.snapshot.bin
```

To use the recovered state, rename `recovered.snapshot.bin` to `my_database.log.snapshot.bin` and restart the server.

#### Manual Snapshot

You can trigger an on-demand snapshot via the API. This is useful before performing risky operations.

```http
POST /snapshot
Authorization: Bearer <jwt>
```

---

## HTTP API

### Authentication & Token Delegation

```http
POST /login
Content-Type: application/json

{ "username": "admin", "password": "admin123" }
```

Returns `{ "token": "<jwt>" }`. The root token carries the `*:*:*` scope (full admin access). Include it in all
subsequent requests:

```http
Authorization: Bearer <jwt>
```

### Delegate a scoped token (`POST /auth/delegate`)

The root user can mint narrow-permission JWTs for clients. Only the root user can call this endpoint.

```http
POST /auth/delegate
Authorization: Bearer <root-jwt>
Content-Type: application/json

{
  "client_id": "laptop-service",
  "scopes": ["read:laptops:*", "write:laptops:*"],
  "ttl_secs": 3600
}
```

Returns `{ "token": "<scoped-jwt>", "jti": "<uuid>", "client_id": "laptop-service", "scopes": [...] }`. Save the `jti` —
you will need it to revoke this token.

**Scope format:** `action:collection:document_key`

| Scope              | Meaning                               |
|--------------------|---------------------------------------|
| `read:laptops:lp1` | Read only document `lp1` in `laptops` |
| `read:laptops:*`   | Read any document in `laptops`        |
| `write:laptops:*`  | Write any document in `laptops`       |
| `delete:laptops:*` | Delete any document in `laptops`      |
| `read:*:*`         | Read any document in any collection   |
| `*:*:*`            | Full admin — root only                |

Every endpoint enforces scopes. A token missing the required scope receives `403 Forbidden`.

### Revoke a token (`DELETE /auth/tokens/:jti`)

Immediately invalidates a JWT before its TTL expires. Requires a `*:*:*` (admin) token.

```http
DELETE /auth/tokens/da5e155d-6a47-48eb-9cc3-7fc440cd2e39
Authorization: Bearer <admin-jwt>
Content-Type: application/json

{ "exp": 1777562342 }
```

The `exp` value is the Unix timestamp from the token's payload — used as the prune deadline so the revocation entry is
cleaned up automatically once the token would have expired anyway. The revocation store is persisted to
`<db-path>.revocations.json` and reloaded on server restart, so revocations survive restarts.

To extract the `jti` from a token, decode the middle segment of the JWT (base64url) or read it directly from the
`POST /auth/delegate` response.

### Insert / upsert

```http
POST /set
Authorization: Bearer <jwt>
Content-Type: application/json

{
  "collection": "users",
  "data": {
    "u1": { "name": "Alice", "role": "admin" },
    "u2": { "name": "Bob",   "role": "viewer" }
  }
}
```

### Query

```http
POST /get
Authorization: Bearer <jwt>
Content-Type: application/json

{
  "collection": "users",
  "where": { "role": "admin" },
  "fields": ["name", "role"],
  "sort": [{ "field": "name", "order": "asc" }],
  "count": 10,
  "offset": 0
}
```

### Update (partial patch)

```http
POST /update
Authorization: Bearer <jwt>
Content-Type: application/json

{ "collection": "users", "data": { "u1": { "role": "superadmin" } } }
```

### Delete

```http
POST /delete
Authorization: Bearer <jwt>
Content-Type: application/json

{ "collection": "users", "keys": "u1" }
```

### REST-style GET

```http
# Fetch a single document (requires read:users:u1 or read:users:* or *:*:*)
GET /collections/users/docs/u1
Authorization: Bearer <jwt>

# Fetch all documents in a collection (requires read:users:* or *:*:*)
GET /collections/users?limit=100&offset=0
Authorization: Bearer <jwt>
```

### WebSocket

```
wss://localhost:1538/ws
```

Emits a JSON message for every mutation:

```json
{
  "event": "change",
  "collection": "users",
  "key": "u1",
  "new_v": 2
}
```

---

## Testing

A full API walkthrough — including all six privilege levels from document-level read to full admin — is available in [
`tests/requests.http`](../tests/requests.http). Run it with any HTTP client that supports `.http` files (JetBrains HTTP
Client, REST Client for VS Code, etc.).

For a quick test, start the server with `--root-user admin --root-password admin123`, log in via `POST /login`, and
paste the token into the `@token` variable at the top of the file.

---

## Making auth optional

`moltendb-auth` handles all identity concerns — JWT minting, Argon2 hashing, scope enforcement. MoltenDB is designed to
work alongside your own user table: your backend validates credentials against your database, then calls
`POST /auth/delegate` with the root token to mint a scoped JWT for the client. The root token never leaves your backend.

See the [`moltendb-auth` README](../moltendb-auth/README.md) for the full integration pattern and scope reference.

---

## Part of the MoltenDB workspace

```
MoltenDB/
├── moltendb-core/     — pure engine (DashMap, WAL, query evaluator)
├── moltendb-wasm/     — browser adapter (wasm-bindgen glue, WorkerDb, OPFS)
├── moltendb-auth/     — identity layer (JWT, Argon2, UserStore)
└── moltendb-server/   ← you are here
```

See the [root README](../README.md) for the full architecture overview and feature list.
