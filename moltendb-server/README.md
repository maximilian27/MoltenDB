<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-server

### 🚀 The Network Layer Crate

**Axum HTTP server · TLS · JWT auth · CORS · Rate limiting · WebSocket · CLI config**  
The runnable binary. Delegates all database logic to `moltendb-core`.

[![License](https://img.shields.io/badge/license-BSL%201.1-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-56%20passing-brightgreen?style=flat-square)](#testing)

</div>

---

## What is this crate?

`moltendb-server` is the runnable binary of MoltenDB. It owns everything related to the network layer and leaves all database logic to `moltendb-core`:

- **Axum HTTP server** with TLS termination (`axum-server` + `rustls`).
- **REST endpoints** — `POST /set`, `POST /get`, `POST /update`, `POST /delete`, `POST /snapshot`, `POST /analytics` (⚠️ analytics under development).
- **REST-style GET** — `GET /:collection/:key` and `GET /:collection` (with query string filters).
- **WebSocket** — `GET /ws` for real-time mutation notifications.
- **CLI Utility** — `moltendb recover` subcommand for Point-in-Time Recovery.
- **JWT auth middleware** — all protected routes require a valid `Authorization: Bearer <token>` header.
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
  --storage-mode tiered \
  --jwt-secret my-awesome-secret \
  --admin-user admin \
  --admin-password admin123 \
  --cors-origin "*"
```

Or with `cargo run` from the workspace root:

```bash
cargo run --package moltendb-server --bin moltendb -- \
  --db-path my_database.log \
  --write-mode async \
  --storage-mode tiered \
  --jwt-secret my-awesome-secret \
  --admin-user admin \
  --admin-password admin123 \
  --cors-origin "*"
```

---

## CLI reference

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--port` | `PORT` | `1538` | Port to listen on |
| `--db-path` | `DB_PATH` | `my_database.log` | Path to the WAL log file |
| `--cert` | `TLS_CERT` | `cert.pem` | TLS certificate PEM file |
| `--key` | `TLS_KEY` | `key.pem` | TLS private key PEM file |
| `--write-mode` | `WRITE_MODE` | `async` | `async` (blazing fast, buffered) or `sync` (durable, flushed to disk). `async` is recommended for most web use-cases, while `sync` is better for mission-critical data. |
| `--storage-mode` | `STORAGE_MODE` | `standard` | `standard` or `tiered` (hot + cold log, recommended for 100k+ docs) |
| `--encryption-key` | `ENCRYPTION_KEY` | — | At-rest encryption password (ChaCha20-Poly1305) |
| `--jwt-secret` | `JWT_SECRET` | **required** | JWT signing secret — server refuses to start without it |
| `--admin-user` | `MOLTENDB_ADMIN_USER` | — | Admin username seeded at startup |
| `--admin-password` | `MOLTENDB_ADMIN_PASSWORD` | — | Admin password seeded at startup |
| `--cors-origin` | `CORS_ORIGIN` | `*` | Allowed CORS origin(s), comma-separated |
| `--rate-limit-requests` | `RATE_LIMIT_REQUESTS` | `100` | Max requests per IP per window |
| `--rate-limit-window` | `RATE_LIMIT_WINDOW_SECS` | `60` | Rate limit sliding window in seconds |
| `--max-body-size` | `MAX_BODY_SIZE` | `10485760` | Max request body size in bytes (default 10 MB) |
| `--hot-threshold` | `MOLTEN_HOT_THRESHOLD` | `50000` | Number of documents per collection kept in RAM before paging out to disk |
| `--disable-encryption` | — | — | Disable at-rest encryption |

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

### Authentication

```http
POST /login
Content-Type: application/json

{ "username": "admin", "password": "admin123" }
```

Returns `{ "token": "<jwt>" }`. Include the token in all subsequent requests:

```http
Authorization: Bearer <jwt>
```

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
GET /users/u1
GET /users
Authorization: Bearer <jwt>
```

### WebSocket

```
wss://localhost:1538/ws
```

Emits a JSON message for every mutation:

```json
{ "event": "change", "collection": "users", "key": "u1", "new_v": 2 }
```

---

## Testing

A full API walkthrough is available in [`tests/requests.http`](../tests/requests.http). Run it with any HTTP client that supports `.http` files (JetBrains HTTP Client, REST Client for VS Code, etc.).

---

## Making auth optional

`moltendb-auth` is compiled behind the `auth` Cargo feature, which is enabled by default. To bring your own auth layer, disable the default feature:

```toml
[dependencies]
moltendb-server = { version = "0.6.2", default-features = false }
```

Then wrap the Axum router with your own `tower::Layer` before calling `serve()`. Without the `auth` feature the server starts without requiring `--jwt-secret` and all routes are unprotected.

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
