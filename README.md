<div align="center">
  <img src="assets/logo.png" alt="MoltenDB Logo" width="400"/>

# MoltenDB

### 🌋 A Universal Local-First Database in Pure Rust

**Runs in the browser (WASM + OPFS) and on the server (Rust + disk).**  
Same query engine. Same Bitcask-inspired hybrid storage. Two environments.

**Request only the fields you need — like GraphQL, but over a plain JSON API.**

[![License](https://img.shields.io/badge/license-BSL%201.1-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-80%20passing-brightgreen?style=flat-square)](#testing)

**⚠️ Beta Software** — APIs may change. Not recommended for production use yet.

</div>

---

## What is MoltenDB?

MoltenDB is a JSON document database written in Rust that compiles to both a native server binary and a WebAssembly module. The same query engine runs in your browser (via WASM + OPFS) and on your server (via a Rust binary + disk). Data written in the browser persists across page reloads and can optionally sync to the server.

---

## Architecture

MoltenDB is structured as a **Cargo Workspace** with four independent crates. Each crate has a single, well-defined responsibility and can be used in isolation.

```
MoltenDB/
├── moltendb-core/     — pure engine: no HTTP, no auth, no JWT, no WASM bindings
├── moltendb-wasm/     — browser adapter: wasm-bindgen glue, WorkerDb, OPFS
├── moltendb-auth/     — identity layer: JWT, Argon2, UserStore
└── moltendb-server/   — network layer: Axum, TLS, CORS, CLI config
```

### `moltendb-core` — The Engine

The heart of MoltenDB. Contains the in-memory `DashMap` store, the append-only WAL, all storage backends (disk, tiered, encrypted, OPFS), the query evaluator (`$in`, `$gt`, joins, field projection), auto-indexing, and all handler and validation logic shared between the server and the WASM adapter.

**Zero knowledge of HTTP, TCP, JWT, users, or WASM bindings.** This crate compiles to:
- A native `rlib` for embedding in other Rust projects
- A `cdylib` for FFI (mobile, Tauri, etc.)

### `moltendb-wasm` — The Browser Adapter

A thin `cdylib` crate that wraps `moltendb-core` and exposes it to JavaScript via `wasm-bindgen`. Contains `WorkerDb` — the WASM entry point used by the Web Worker — and all browser-specific glue (`web-sys`, `js-sys`, OPFS access). Built with `wasm-pack build moltendb-wasm --target web`.

**JS initialisation** uses a named static factory (not an async constructor, which produces invalid TypeScript):
```js
// ✅ correct
const db = await WorkerDb.create("my_database");

// ❌ deprecated — do not use
const db = await new WorkerDb("my_database");
```

Keeping WASM bindings in a separate crate means `moltendb-core` and `moltendb-server` have a clean, WASM-free dependency tree.

**Use it as an embedded database** — add it to any Rust project with no HTTP overhead:

```toml
# Cargo.toml
[dependencies]
moltendb-core = "0.8.0"
```

```rust
use moltendb_core::engine::{Db, DbConfig};

let config = DbConfig {
    path: "./my_app.log".to_string(),
    sync_mode: true,
    hot_threshold: 1000,
    ..Default::default()
};

let db = Db::open(config).await?;
db.insert_batch("users", vec![("u1".to_string(), serde_json::json!({ "name": "Alice" }))])?;
let user = db.get("users", "u1");
```

| Feature | Available in `moltendb-core`? | Available in `moltendb-server`? | Why? |
| :--- | :--- | :--- | :--- |
| `MOLTENDB_DB_PATH` | No (passed via `DbConfig`) | **Yes** | Engine needs a path; server provides the CLI flag. |
| `MOLTENDB_PORT` | **No** | **Yes** | Core has no network listener or HTTP logic. |
| `MOLTENDB_ROOT_USER` | **No** | **Yes** | Core doesn't handle API authentication. |
| `MOLTENDB_JWT_SECRET` | **No** | **Yes** | Server-side token security. |
| `MOLTENDB_SYNC_MODE` | No (passed via `DbConfig`) | **Yes** | Controls the core engine's storage behavior. |

> [!TIP]
> **When using the standalone `moltendb-server` binary, all flags and environment variables are available.** The server acts as a thin wrapper that combines the engine, authentication, and networking layers. The distinction only matters if you are using `moltendb-core` as a library in your own Rust project.

### 3. How to configure `moltendb-core` directly

If you are building a custom application and importing `moltendb-core`, you don't use environment variables or CLI flags unless you implement them yourself. Instead, you initialize the database using the `DbConfig` struct:

```rust
use moltendb_core::engine::{Db, DbConfig};

#[tokio::main]
async fn main() {
    // Core doesn't know about MOLTENDB_PORT or MOLTENDB_ROOT_USER
    let config = DbConfig {
        path: "my_data.db".to_string(),
        sync_mode: true,
        hot_threshold: 10000,
        ..Default::default()
    };

    let db = Db::open(config).await.unwrap();
    // Now you have a running database instance in your own app!
}
```

In summary: **the server flags are just a user interface for the standalone binary.** If you use the core package as a library, you are responsible for how you want to configure it.

---

### `moltendb-auth` — The Identity Layer

Handles everything related to identity: Argon2 password hashing, JWT minting and validation (HMAC-SHA256), the `UserStore`, and **scoped token delegation**. Depends only on `moltendb-core` — it has no knowledge of HTTP routing or the server binary.

**Single root user.** One root user is configured at startup via `--root-user` / `--root-password`. There is no user management API — MoltenDB is designed to work alongside your own user table. Your backend validates credentials against your database, then calls `POST /auth/delegate` to mint a narrow-scoped JWT for the client. The root token never leaves your backend.

**WASM excluded.** The entire crate is gated with `#![cfg(not(target_arch = "wasm32"))]` — auth is irrelevant for local browser storage and adds no weight to the WASM bundle.

### `moltendb-server` — The Network Layer

The runnable binary. Owns Axum routing, TLS termination, CORS policy, per-IP rate limiting, HTTP body size enforcement, and the CLI configuration (via `clap`). Parses incoming JSON requests and delegates to `moltendb-core`. Depends on both `moltendb-core` and `moltendb-auth`.

---

> **Deployment model:** Run `moltendb-server` as a standalone HTTPS server, embed `moltendb-core` directly in your Rust application, or compile `moltendb-core` to WASM for browser-side local-first storage.

MoltenDB uses a **Hybrid Bitcask-inspired Storage Model**. Frequently accessed data is kept in RAM (`Hot`) as parsed JSON for sub-microsecond reads. Less frequently used data is paged out to disk (`Cold`) as byte-offsets, freeing up memory. This allows MoltenDB to handle datasets much larger than the available RAM while maintaining high performance for the active working set.

By default, any collection exceeding **50,000 documents** will automatically evict the oldest documents to the `Cold` tier (disk/OPFS). Cold documents are fetched and deserialized on-demand, which typically takes ~50µs. This limit is configurable via `--hot-threshold` (server) or as an argument to `WorkerDb.create` (WASM).

One of MoltenDB's core features is **GraphQL-style field selection**: every query lets you specify exactly which fields (including deeply nested ones) you want back. You never receive more data than you asked for — no over-fetching, no under-fetching, no separate schema to maintain.

---

### Examples

- **[TODO App (CLI)](moltendb-core/examples/todo_app.rs)** — A simple CLI application demonstrating basic CRUD operations and data persistence using only `moltendb-core`.
  ```bash
  cargo run -p moltendb-core --example todo_app
  ```

- **[TODO App (Desktop)](moltendb-core/examples/todo_desktop.rs)** — A standalone GUI application built with `eframe` (egui) and `moltendb-core`.
  ```bash
  cargo run -p moltendb-core --example todo_desktop
  ```

## What Actually Works Today

### ✅ Browser (WASM + OPFS)
- Full document store running inside a Web Worker — zero main-thread blocking
- Data persists across page reloads using the Origin Private File System (OPFS)
- Automatic log compaction: count-based (every 500 inserts) and size-based (> 5 MB)
- **[`@moltendb-web/core` on NPM](https://www.npmjs.com/package/@moltendb-web/core)** — bundles the WASM engine, Web Worker, and main-thread client into a single publishable artifact
- **[`@moltendb-web/query` on NPM](https://www.npmjs.com/package/@moltendb-web/query)** — type-safe, chainable query builder (CJS + ESM + `.d.ts`)
- **[`@moltendb-web/angular` on NPM](https://www.npmjs.com/package/@moltendb-web/angular)** — official Angular wrapper for seamless integration
- **Point-in-Time Recovery Ready:** Every write in the browser now includes a `_t` timestamp. While the recovery tool runs natively, browser logs can be exported and recovered to any millisecond using the native CLI.
- **[⚡ Try the Live Angular Demo](https://moltendb-angular.maximilian-both27.workers.dev/laptops)**
- **[⚡ Try the Live Browser WASM Demo on StackBlitz](https://stackblitz.com/~/github.com/maximilian27/moltendb-wasm-demo)**

### ✅ Server (Rust binary)
- HTTPS-only server with TLS (cert + key required)
- JWT authentication (`POST /login` → bearer token)
- Per-IP sliding-window rate limiting
- At-rest encryption with XChaCha20-Poly1305 (on by default, key from `--encryption-key`)
- **In-memory store:** the entire dataset lives in RAM (`DashMap`) — reads are pure hashmap lookups with no disk I/O; RAM is the hard dataset size limit
- Two write modes: async (50 ms flush, high throughput) and sync (flush-on-write, zero data loss)
- Two storage modes: standard (single log file) and tiered (hot + cold log, mmap cold reads)
- Binary snapshots on compaction for fast startup (snapshot + delta replay, not full log replay)
- **Point-in-Time Recovery (PITR):** Recover the database to any millisecond or log sequence number using the `recover` CLI command.
- **Snapshot Versioning:** Historical snapshots are automatically moved to a `/backup` folder with Unix timestamps.
- **Post-Backup Hook:** Automatically execute custom shell commands (e.g., S3 upload, Slack notify) after every successful snapshot.
- **Manual Snapshots:** Trigger a snapshot on demand via the `POST /snapshot` endpoint.
- Size-based compaction trigger (> 100 MB) in addition to the hourly timer
- WebSocket endpoint (`/ws`) for real-time push notifications — subscribe and receive change events on every write

### ✅ Query Engine (shared between browser and server)
- **GraphQL-style field selection** — request only the fields you need using `fields` (include) or `excludedFields` (exclude). Dot-notation works at any depth: `"specs.display.features.refresh_rate"` returns only that one nested value, not the whole document.
- `WHERE` clause with: `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$contains` / `$ct` (strings and arrays), `$in` / `$oneOf`, `$nin` / `$notIn` — all string comparisons are **case-insensitive**
- Field projection (`fields`) and field exclusion (`excludedFields`) — mutually exclusive, validated before any data is read
- Pagination: `count` (limit) and `offset`
- Cross-collection joins with dot-notation foreign keys
- Auto-indexing: fields queried 3+ times get an index automatically; equality lookups become O(1)
- Range query index acceleration: `$gt`/`$lt` scan the index values instead of all documents
- **Snapshot Exports:** Atomic, non-blocking binary snapshots for fast recovery and off-site backups.
- **JSON Schema Validation:** High-speed consistency enforcement on a per-collection basis.
- **Optimistic Concurrency Control:** Improved version conflict detection and `409 Conflict` reporting.
- **Document versioning:** every document automatically gets `_v`, `createdAt`, `modifiedAt`
- **Atomic Batch Transactions:** WAL transaction markers (`TX_BEGIN`/`TX_COMMIT`) prevent partial write failures.
- Conflict resolution: incoming writes with stale `_v` return a `409 Conflict` error.
- Inline reference embedding (`extends`): embed data from another collection at insert time

### ✅ Security
- Passwords hashed with Argon2id
- JWT tokens signed with HMAC-SHA256; root tokens carry `*:*:*` scope (24-hour expiry)
- **Scoped token delegation:** root user mints narrow-permission JWTs for clients via `POST /auth/delegate`. Scope format: `action:collection:document_key` (e.g. `read:laptops:lp1`, `write:users:*`, `read:*:*`). Every endpoint enforces scopes — tokens missing the required scope receive `403 Forbidden`.
- **Document-level access control:** a token with `read:laptops:lp1` can only read that one document. `POST /get` without a key filter automatically returns only the documents the token is permitted to see.
- **Only the root user can mint `*:*:*` (admin) tokens** — non-root admin tokens cannot escalate their own privileges.
- **Token revocation (JTI blacklist):** every JWT carries a unique `jti` (UUID). Compromised or leaked tokens can be immediately invalidated via `DELETE /auth/tokens/:jti` (admin-only) before their TTL expires. The revocation store is persisted to `<db-path>.revocations.json` and reloaded on server restart — revocations survive restarts.
- Credentials loaded from environment variables at startup (no hardcoded defaults in production)
- **Single root user:** MoltenDB supports exactly one root user. Your own user table handles the rest — MoltenDB never stores your application users.
- Input validation: collection names, key names, field names, JSON depth (max 32), payload size (max 10 MB), batch size (max 1000 keys)
- Security headers on every response: `X-Content-Type-Options`, `X-Frame-Options`, `HSTS`, `CSP`, etc.
- Graceful shutdown: drains in-flight requests (up to 30 s), then awaits the async writer task to fully flush all buffered log entries before exit

### ✅ Developer Tooling
- **Interactive WASM Browser Demo** — A complete, live environment to test raw JSON queries and the chainable builder directly in your browser.
  - [Run Live on StackBlitz](https://stackblitz.com/~/github.com/maximilian27/moltendb-wasm-demo) (Zero setup required)
  - [View WASM Demo Source Code (GitHub)](https://github.com/maximilian27/moltendb-wasm-demo)
- **[Server Integration Test Suite (GitHub)](https://github.com/maximilian27/moltendb-server-test)** — A browser-based testing environment to exercise the HTTP API and WebSocket endpoint against a live server using the TypeScript client. Includes an interactive Server Query Builder, a WebSocket tester, and a collection fetcher.
- **57+ documented example requests** in `tests/requests.http`
- **80+ integration tests** covering all query features, versioning, persistence, compaction, concurrency, and schema validation.
- **Rust stress-test examples** (`examples/`) — generate 100 000 synthetic documents, bulk-insert via HTTP, and run 10 000–100 000 concurrent fetch requests with a full latency percentile report.

---

## Getting Started

### Prerequisites

- Rust 1.85+ (`rustup update stable`)
- Node.js 20+ (for the dev server and npm packages)
- `wasm-pack` (only if building the browser package: `cargo install wasm-pack`)
- A TLS certificate and key (for the server)

### Install via Cargo (Easiest)

If you just want to run the standalone database server, install it directly from crates.io:

```bash
cargo install moltendb-server
```

### Use the core engine as an embedded library

Add `moltendb-core` to your `Cargo.toml` to embed the engine directly — no HTTP server, no auth overhead:

```toml
[dependencies]
moltendb-core = "0.8.0"
```

### Download Pre-built Binaries

Alternatively, you can also download the pre-compiled binaries and self-signed certificates directly from the GitHub releases page.

### Generate a self-signed certificate (development only)

```bash
openssl req -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem -days 365 -nodes \
  -subj "/CN=localhost"
```

### Build the WASM package

The WASM package targets `moltendb-core` only — no HTTP or auth deps are included:

```bash
wasm-pack build moltendb-wasm --target web
```

### Run the server

```bash
# Set credentials (REQUIRED)
export MOLTENDB_ROOT_USER=myuser
export MOLTENDB_ROOT_PASSWORD=str0ng-p4ssw0rd
export MOLTENDB_JWT_SECRET=another-strong-secret

# Run the server binary
cargo run --release -p moltendb-server

# Or with CLI flags (equivalent)
cargo run --release -p moltendb-server -- \
  --root-user myuser \
  --root-password str0ng-p4ssw0rd \
  --jwt-secret another-strong-secret \
  --encryption-key my-encryption-password \
  --port 1538

# Verbose debug logging (optimizer, indexing, compaction details)
cargo run --release -p moltendb-server -- --debug
```

Run `cargo run -p moltendb-server -- --help` to see all available flags.


### Quick Test with `requests.http`

If you want to quickly test the functionality with the requests.http file, you should start the server with the following credentials (via CLI flags or environment variables): \
  **--root-user `admin`**\
  **--root-password `admin123`**\
Make sure to login first and then replace the token in the requests.http file with the one you get from the login response.

### RECOVERY & MAINTENANCE

#### Take a manual snapshot
```http
POST /snapshot
Authorization: Bearer <token>
```
Triggers an immediate compaction and saves a new `snapshot.bin`. The previous snapshot is moved to the `/backup` folder.

#### Point-in-Time Recovery (CLI)
To recover a database to a specific time (e.g., before a bug deleted data):
```bash
moltendb recover --log my_database.log --to-time 1713972000000 --out recovered.snapshot.bin
```
The resulting `recovered.snapshot.bin` can then be renamed to `my_database.log.snapshot.bin` to restore the state.

---

## HTTP API

All endpoints except `POST /login` require an `Authorization: Bearer <token>` header. Every endpoint also enforces **scopes** — the token must carry the appropriate `action:collection:key` scope or the request is rejected with `403 Forbidden`.  
All endpoints return a consistent JSON envelope with a `statusCode` field:

```json
{ "statusCode": 200, "count": 5, "status": "ok" }
```
```json
{ "statusCode": 400, "error": "Unknown property: 'foo'. Check the API docs..." }
```
```json
{ "statusCode": 404, "error": "No documents found" }
```

### Authentication

```http
POST /login
Content-Type: application/json

{ "username": "myuser", "password": "str0ng-p4ssw0rd" }
```

Returns `{ "token": "<jwt>" }`. The root token carries `*:*:*` scope (full access).

### Delegate a scoped token

The root user can mint narrow-permission JWTs for clients. Only the root user can call this endpoint.

```http
POST /auth/delegate
Authorization: Bearer <root-token>
Content-Type: application/json

{
  "client_id": "laptop-service",
  "scopes": ["read:laptops:*", "write:laptops:*"],
  "ttl_secs": 3600
}
```

Returns `{ "token": "<scoped-jwt>", "client_id": "laptop-service", "scopes": [...] }`.

**Scope format:** `action:collection:document_key`

| Scope | Meaning |
|---|---|
| `read:laptops:lp1` | Read only document `lp1` in `laptops` |
| `read:laptops:*` | Read any document in `laptops` |
| `write:laptops:*` | Write any document in `laptops` |
| `delete:laptops:*` | Delete any document in `laptops` |
| `read:*:*` | Read any document in any collection |
| `*:*:*` | Full admin — root only |

### Insert / Upsert

```http
POST /set
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "data": {
    "lp1": { "brand": "Lenovo", "model": "ThinkPad X1 Carbon", "price": 1499, "in_stock": true }
  }
}
```

Pass `data` as an **array** to auto-generate UUIDv7 keys:

```json
{ "collection": "laptops", "data": [{ "brand": "HP", "model": "Spectre x360", "price": 1599 }] }
```

Returns `{ "statusCode": 200, "status": "ok", "count": 1 }`.

Every document automatically receives `_v` (version counter), `createdAt`, and `modifiedAt` fields managed by the engine.

### Query

```http
POST /get
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "where": { "brand": { "$in": ["Apple", "Dell"] }, "in_stock": true },
  "fields": ["brand", "model", "price"],
  "count": 10,
  "offset": 0
}
```

**All query properties:**

| Property | Type | Description                                                                                                                                |
|---|---|--------------------------------------------------------------------------------------------------------------------------------------------|
| `collection` | string | **Required.** The collection to query.                                                                                                     |
| `keys` | string \| string[] | Fetch one or more documents by key. Returns the document directly for a single string; returns an array for an array of keys.              |
| `where` | object | Filter documents. All conditions at the top level are ANDed together.                                                                      |
| `fields` | string[] | **GraphQL-style field selection.** Return only these fields. Dot-notation selects nested fields. Mutually exclusive with `excludedFields`. |
| `excludedFields` | string[] | Return everything *except* these fields. Mutually exclusive with `fields`.                                                                 |
| `joins` | object[] | Cross-collection joins. Each element is `{ "<name>": { "from": "<collection>", "on": "<foreign_key_field>", "fields": [...] } }`.          |
| `sort` | object[] | Sort results. Each spec is `{ "field": "<name>", "order": "asc" \| "desc" }`. Multiple specs applied in priority order.                    |
| `count` | number | Maximum number of results to return (applied after filtering and sorting).                                                                 |
| `offset` | number | Number of results to skip (for stable pagination, applied after sorting).                                                                  |

> **Response shape:** All multi-document queries return a **JSON array** where each element includes a `_key` field with the document ID. The only exception is a single-key lookup (`"keys": "lp2"`) which returns the document directly.

**Supported `where` operators:**

| Operator | Aliases | Description |
|---|---|---|
| `$eq` | `$equals` | Exact equality |
| `$ne` | `$notEquals` | Not equal |
| `$gt` | `$greaterThan` | Greater than (numeric) |
| `$gte` | | Greater than or equal |
| `$lt` | `$lessThan` | Less than (numeric) |
| `$lte` | | Less than or equal |
| `$contains` | `$ct` | Substring check (string, **case-insensitive**) or membership check (array) |
| `$in` | `$oneOf` | Field value is one of a list (string comparison is **case-insensitive**) |
| `$nin` | `$notIn` | Field value is not in a list |
| `$or` | | At least one of the sub-conditions must match (array of `where`-style objects) |
| `$and` | | All sub-conditions must match (array of `where`-style objects) |

**Query examples:**

// WHERE with multiple conditions (all must match — implicit AND)
```json
{ "collection": "laptops", "where": { "brand": "Apple", "in_stock": true } }
```
// GraphQL-style field selection
```json
{ "collection": "laptops", "fields": ["brand", "model", "price"] }
```
// Deep nested field selection
```json
{ "collection": "laptops", "fields": ["brand", "specs.cpu.ghz", "specs.weight_kg"] }
```
// Field exclusion
```json
{ "collection": "laptops", "excludedFields": ["memory_id", "display_id"] }
```
// Sort by price descending, then brand ascending
```json
{ "collection": "laptops", "sort": [{ "field": "price", "order": "desc" }, { "field": "brand", "order": "asc" }] }
```
// Pagination — second page of 3
```json
{ "collection": "laptops", "sort": [{ "field": "price", "order": "asc" }], "offset": 3, "count": 3 }
```
// $in — brand is one of a list
```json
{ "collection": "laptops", "where": { "brand": { "$in": ["Apple", "Dell", "Razer"] } } }
```
// $contains on an array field
```json
{ "collection": "laptops", "where": { "tags": { "$contains": "gaming" } } }
```
// $or — match documents where brand is Apple OR price is below 1000
```json
{ "collection": "laptops", "where": { "$or": [{ "brand": "Apple" }, { "price": { "$lt": 1000 } }] } }
```
// $and — match documents where brand is Apple AND price is below 2000
```json
{ "collection": "laptops", "where": { "$and": [{ "brand": "Apple" }, { "price": { "$lt": 2000 } }] } }
```

### Cross-collection join

```http
POST /get
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "fields": ["brand", "model", "price"],
  "joins": [
    {  
      "ram": { 
        "from": "memory", 
        "on": "memory_id", 
        "fields": ["capacity_gb", "type"] 
      }
    },
    { 
      "screen": { 
        "from": "display",
        "on": "display_id", 
        "fields": ["size_inch", "panel", "refresh_hz"]
      }
    }
  ]
}
```

The `on` field is read from the parent document using dot-notation and used to look up a document in the target collection. The result is embedded under the alias key. `fields` is optional — omit it to return the full joined document.

> **Note:** Joins are resolved at **query time** — the joined data is fetched live on every request. For a snapshot embedded at **insert time**, use `extends` (see below).

### Inline reference embedding (`extends`)

The `extends` key embeds data from another collection directly into the stored document at insert time — no join needed on reads.

```http
POST /set
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "data": {
    "lp7": {
      "brand": "MSI",
      "model": "Titan GT77",
      "price": 3299,
      "extends": {
        "ram":    "memory.mem4",
        "screen": "display.dsp3"
      }
    }
  }
}
```

Each value in `extends` is a `"collection.key"` reference. The engine fetches the referenced document and embeds it under the alias key. The `extends` key itself is removed from the stored document.

**When to use `extends` vs `joins`:**

| | `extends` | `joins` |
|---|---|---|
| Resolved at | Insert time (once) | Query time (every request) |
| Data freshness | Snapshot — may become stale | Always live |
| Read cost | O(1) — data already embedded | O(1) per join per document |
| Use when | Data rarely changes, fast reads matter | Data changes frequently, freshness matters |

### Patch / merge

```http
POST /update
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "data": { "lp4": { "in_stock": true, "price": 1749 } }
}
```

Only the fields in `data` are changed. All other fields are preserved. `_v` is incremented automatically; `createdAt` cannot be overwritten.

### Delete

```http
POST /delete
Content-Type: application/json
Authorization: Bearer <token>

{ "collection": "laptops", "keys": "lp6" }              // single key
{ "collection": "laptops", "keys": ["lp4", "lp5"] }     // batch
{ "collection": "laptops", "drop": true }               // drop entire collection
```

### Paginated collection fetch

```http
GET /collections/laptops?limit=100&offset=0
Authorization: Bearer <token>
```

Returns all documents in the collection, with optional pagination.

---

## Query Builder (JavaScript / TypeScript)

The `@moltendb-web/query` package provides a type-safe, chainable API that works with both the HTTP server and the WASM engine.

```bash
npm install @moltendb-web/query
```

```typescript
import { MoltenDBClient, WorkerTransport, HttpTransport } from '@moltendb-web/query';

// WASM (browser)
const client = new MoltenDBClient(new WorkerTransport(worker));

// HTTP server
const client = new MoltenDBClient(new HttpTransport('https://localhost:1538', token));

// GET — chainable query
const results = await client.collection('laptops')
  .get()
  .where({ brand: 'Apple', in_stock: true })
  .fields(['brand', 'model', 'price'])
  .joins([{ 
    screen: { 
      from: 'display', on: 'display_id', fields: ['panel', 'refresh_hz'] 
    }
  }])
  .sort([{ field: 'price', order: 'asc' }])
  .count(5)
  .exec();

// SET — insert / upsert
await client.collection('laptops')
  .set({ lp1: { brand: 'Lenovo', model: 'ThinkPad X1', price: 1499 } })
  .exec();

// UPDATE — partial patch
await client.collection('laptops')
  .update({ lp4: { price: 1749, in_stock: true } })
  .exec();

// DELETE
await client.collection('laptops').delete().keys('lp6').exec();
await client.collection('laptops').delete().drop().exec();
```

Each operation class only exposes the methods that are valid for that operation — invalid method chains are caught at compile time in TypeScript.

---

## WebSocket (Real-time Push)

The WebSocket endpoint is exclusively for **real-time push notifications**. All CRUD operations must go through the HTTP endpoints.

```
wss://localhost:1538/ws
```

**Protocol:**

1. The first message **must** be `{ "action": "AUTH", "token": "<jwt>" }`. The connection is closed immediately if authentication fails.
2. After authentication, the server pushes a change event on every write:
   ```json
   { "event": "change", "collection": "laptops", "key": "lp2", "new_v": 3 }
   ```
   ```json
   { "event": "change", "collection": "laptops", "key": "lp6", "new_v": null }
   ```
   ```json
   { "event": "change", "collection": "laptops", "key": "*",   "new_v": null }
   ```
   - `new_v` is the document's `_v` after the write, or `null` for deletes/drops
   - `key: "*"` means the entire collection was dropped
3. Clients fetch fresh data via HTTP after receiving a notification.

See `src/ws_test/websocket-test.html` for an interactive tester.

---

## Telemetry

### Health check

Public endpoint — no authentication required. Use it as a liveness probe in Docker / Kubernetes.

```http
GET /system/health
```

Response:
```json
{ "status": "ok", "message": "MoltenDB is running" }
```

### Metrics

Admin-only endpoint. Returns a structured snapshot of server uptime, process memory, host hardware, and live database internals. All values are raw integers — formatting is left to the client (MoltenDB Studio / dashboards).

```http
GET /system/metrics
Authorization: Bearer <admin-token>
```

Response:
```json
{
  "uptime_seconds": 14200,
  "process": {
    "memory_used_bytes": 20017152
  },
  "host": {
    "memory": {
      "total_bytes": 34070192128,
      "used_bytes": 17026154496,
      "free_bytes": 17044037632
    },
    "disks": [
      {
        "mount": "C:\\",
        "total_bytes": 1022645760000,
        "used_bytes": 616695963648,
        "available_bytes": 405949796352
      }
    ]
  },
  "database": {
    "hot_keys_count": 14523,
    "hot_tier_threshold": 50000,
    "wal_size_bytes": 8450122,
    "storage_mode": "tiered"
  }
}
```

| Field | Description |
|---|---|
| `uptime_seconds` | Seconds since the server started |
| `process.memory_used_bytes` | RAM consumed by the MoltenDB process |
| `host.memory` | Total / used / free RAM on the host machine |
| `host.disks` | Per-disk total, used, and available bytes |
| `database.hot_keys_count` | Number of documents currently held in the hot (RAM) tier |
| `database.hot_tier_threshold` | Configured max documents per collection before cold eviction |
| `database.wal_size_bytes` | Current size of the WAL / storage file on disk |
| `database.storage_mode` | `standard` or `tiered` |

Returns `403 Forbidden` if the token does not have admin (`*:*:*`) scope.

---

## Configuration Reference

All options can be set via CLI flags or environment variables. CLI flags take priority.

> [!NOTE]
> **If you are running the `moltendb-server` binary, you can use all flags listed below.** The separation between "Networking/Auth" and "Database Engine" is only relevant for developers embedding `moltendb-core` as a library.

### Networking & Authentication (Server-only)
| Flag | Env var | Default | Description |
|---|---|---|---|
| `--cert` | `MOLTENDB_TLS_CERT` | `cert.pem` | TLS certificate |
| `--cors-origin` | `MOLTENDB_CORS_ORIGIN` | `*` ⚠️ | Allowed CORS origin(s) |
| `--jwt-secret` | `MOLTENDB_JWT_SECRET` | **REQUIRED** 🔥 | JWT signing secret |
| `--key` | `MOLTENDB_TLS_KEY` | `key.pem` | TLS private key |
| `--port` | `MOLTENDB_PORT` | `1538` | TCP port |
| `--root-password` | `MOLTENDB_ROOT_PASSWORD` | **REQUIRED** 🔥 | Root password |
| `--root-user` | `MOLTENDB_ROOT_USER` | **REQUIRED** 🔥 | Root username |
| `--debug` | `MOLTENDB_DEBUG` | `false` | Enable verbose debug logging |
| `--dev-mode` | `MOLTENDB_DEV_MODE` | `false` | Run over plain HTTP/WS instead of HTTPS/WSS. Ignores `--cert` and `--key`. ⚠️ NEVER use in production |

### Database Engine Flags (passed to `moltendb-core`)

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--db-path` | `MOLTENDB_DB_PATH` | `my_database.log` | Log file path |
| `--disable-encryption` | `MOLTENDB_DISABLE_ENCRYPTION` | `false` | Store data as plain JSON |
| `--encryption-key` | `MOLTENDB_ENCRYPTION_KEY` | built-in default ⚠️ | At-rest encryption password |
| `--hot-threshold` | `MOLTENDB_HOT_THRESHOLD` | `50000` | Max documents per collection to keep in RAM |
| `--max-body-size` | `MOLTENDB_MAX_BODY_SIZE` | `10485760` | Maximum request body size in bytes |
| `--max-keys-per-request` | `MOLTENDB_MAX_KEYS_PER_REQUEST` | `1000` | Maximum number of keys allowed per JSON request |
| `--post-backup-script` | `MOLTENDB_POST_BACKUP_SCRIPT` | `None` | Path to a script file to run after backup |
| `--rate-limit-requests` | `MOLTENDB_RATE_LIMIT_REQS` | `100` | Max requests per IP per window |
| `--rate-limit-window` | `MOLTENDB_RATE_LIMIT_WINDOW` | `60` | Window size in seconds |
| `--storage-mode` | `MOLTENDB_STORAGE_MODE` | `standard` | `standard` or `tiered` |
| `--write-mode` | `MOLTENDB_WRITE_MODE` | `async` | `async` or `sync` |

### 🔒 Security Considerations

Executing external scripts carries inherent risks. MoltenDB mitigates some of these by:
- **Positional Arguments:** The snapshot path is passed as a sanitized argument, not injected into a command string.
- **Explicit Paths:** On Windows, scripts in the current directory require the `./` prefix (e.g., `--post-backup-script "./my_hook.ps1"`).

#### Recommended Mitigations:
1. **Docker Isolation:** Run MoltenDB in a container to isolate the host filesystem and network. Use a minimal base image.
2. **Principle of Least Privilege:** Run the MoltenDB process under a dedicated service account with access only to its data directory. Ensure only the MoltenDB service user can read the hook script files.
3. **Absolute Paths:** Always use absolute paths for your scripts to avoid "command not found" errors or potential path hijacking.
4. **Sandboxing:** Use `seccomp` or `AppArmor`/`Selinux` on Linux to restrict the types of processes MoltenDB can spawn.
5. **Script Hardening:** Ensure your hook scripts have restricted permissions (e.g., `chmod 700`) and do not contain hardcoded secrets. Use environment variables for API keys.

⚠️ = insecure default, must be overridden in production. The server prints a warning at startup for each one that is not set.

🔥 = mandatory requirement. The server will not start if these are missing.

---

## Storage Modes

### Standard (default)
Single append-only log file. All writes go to `my_database.log`. Compaction rewrites the file to contain only current state (triggered when file > 100 MB or every hour). A binary snapshot is written on each compaction so the next startup only replays the delta, not the full log.

### Tiered (`--storage-mode tiered`)
Recommended for large datasets (100k+ documents). Active writes go to a hot log (kept < 50 MB). When the hot log exceeds the threshold, all current entries are promoted to a cold log (`my_database.cold.log`) which is read via memory-mapped file on startup — the OS pages in only the data actually needed.

### Write modes
- **async** (default): writes are buffered in memory and flushed every 50 ms. Up to 50 ms of data loss on a hard crash. Highest throughput.
- **sync**: every write blocks until the OS confirms the data. Zero data loss on crash. Lower throughput.

---

## How the Log Works

MoltenDB uses an append-only log format — every insert, update, and delete is a new JSON line:

```json
{"cmd":"INSERT","collection":"laptops","key":"lp1","value":{"brand":"Lenovo","model":"ThinkPad X1 Carbon","price":1499,"_v":1,"createdAt":"2026-03-09T13:51:05Z","modifiedAt":"2026-03-09T13:51:05Z"}}
```
```json
{"cmd":"DELETE","collection":"laptops","key":"lp6","value":null}
```
```json
{"cmd":"DROP","collection":"laptops","key":"_","value":null}
```

With encryption enabled (the default), each line is an opaque `ENC` entry:

```json
{"cmd":"ENC","collection":"_","key":"_","value":"base64encodedciphertext..."}
```

On startup, the log is replayed top-to-bottom to rebuild the in-memory state. After compaction, only the current state is kept — dead entries are removed.

---

## Testing

```bash
# Run the full integration test suite (56 tests)
cargo test -p moltendb-server --test integration

# Run with verbose output
cargo test -p moltendb-server --test integration -- --nocapture

# Run the 100 000-entry stress test (insert + log replay verification)
cargo test -p moltendb-server --test stress -- --nocapture
```

The test suite covers: SET, GET, field selection, WHERE (all 9 operators, case-insensitive string matching), sort, pagination, joins, update, delete, versioning, extends, validation, persistence, compaction, concurrency (8 threads × 100 docs), auto-indexing, and analytics.

### Stress & Performance Tools

Three Rust example binaries are provided for real-world load testing against a live server:

```bash
# 1. Generate 100 000 synthetic documents (writes tests/stress_data.json + stress_keys.json)
cargo run -p moltendb-server --example generate_stress_data

# 2. Bulk-insert the dataset into the running server
cargo run -p moltendb-server --example stress_insert

# 3. Fire 10 000 concurrent fetch requests and print a latency report
cargo run -p moltendb-server --example stress_fetch

# Tune concurrency (default 10 000) and collection name via env vars
STRESS_CONCURRENCY=50000 STRESS_COLLECTION=stress cargo run -p moltendb-server --example stress_fetch
```

The fetch report includes min / mean / p50 / p75 / p90 / p95 / p99 / p99.9 / max latency and sustained throughput (req/s). In a typical local debug build, MoltenDB sustains **4 000–8 000 req/s** for pure in-memory reads.

---

## Project Structure

MoltenDB is a Cargo Workspace. Each crate lives in its own directory:

```
MoltenDB/
├── Cargo.toml                        — workspace root
│
├── moltendb-core/                    — pure engine crate (no HTTP, no auth)
│   └── src/
│       ├── lib.rs                    — crate root
│       ├── analytics.rs              — COUNT/SUM/AVG/MIN/MAX analytics engine
│       ├── query.rs                  — query AST evaluator ($eq, $in, $regex, $contains, $or, $and, …)
│       ├── validation.rs             — collection name / document depth / size guards
│       ├── engine/
│       │   ├── mod.rs                — Db struct, open() / open_wasm()
│       │   ├── config.rs             — DbConfig (path, encryption key, storage options)
│       │   ├── indexing.rs           — auto-indexing, query heatmap
│       │   ├── schema.rs             — JSON Schema validation per collection
│       │   ├── types.rs              — LogEntry, DbError
│       │   ├── operations/           — CRUD operations (split module)
│       │   │   ├── mod.rs            — re-exports: get, insert_batch, update, delete, …
│       │   │   ├── common.rs         — shared helpers (now_iso())
│       │   │   ├── read.rs           — get, get_all, get_batch
│       │   │   ├── insert.rs         — insert_batch (versioning, schema validation, WAL)
│       │   │   ├── update.rs         — update (partial patch, _v optimistic lock, WAL)
│       │   │   └── delete.rs         — delete, delete_batch, delete_collection
│       │   └── storage/
│       │       ├── mod.rs            — StorageBackend trait, startup WAL replay
│       │       ├── disk/             — disk storage (split module)
│       │       │   ├── mod.rs        — re-exports: AsyncDiskStorage, SyncDiskStorage, helpers
│       │       │   ├── async_storage.rs — MPSC channel + background Tokio flush task
│       │       │   ├── sync_storage.rs  — Mutex-guarded BufWriter, immediate flush
│       │       │   ├── log.rs        — stream_log_entries, write_compacted_log, count_log_lines
│       │       │   └── snapshot.rs   — write_snapshot, load_snapshot, atomic rename, backup rotation
│       │       ├── encrypted.rs      — XChaCha20-Poly1305 + Argon2id encryption wrapper
│       │       ├── tiered.rs         — TieredStorage, MmapLogReader
│       │       └── wasm.rs           — OpfsStorage (browser OPFS backend)
│       └── handlers/
│           ├── mod.rs
│           ├── process_get.rs        — GET handler (query, field selection, joins, pagination)
│           ├── process_set.rs        — SET handler (insert/upsert, extends resolution)
│           ├── process_update.rs     — UPDATE handler (partial merge, $unset)
│           ├── process_delete.rs     — DELETE handler (single, batch, drop)
│           ├── process_snapshot.rs   — SNAPSHOT handler (PITR trigger)
│           ├── process_schema.rs     — SCHEMA handler (define / update collection schema)
│           └── process_analytics.rs  — ANALYTICS handler (COUNT/SUM/AVG/MIN/MAX)
│
├── moltendb-auth/                    — identity crate (JWT, Argon2, scoped delegation) — excluded from WASM
│   └── src/
│       └── lib.rs                    — Claims (jti, scopes), has_access(), key_matches(),
│                                       create_scoped_token(), RevocationStore,
│                                       UserStore, DelegateRequest/Response,
│                                       auth_middleware (JWT validation + revocation check)
│
├── moltendb-server/                  — network crate (Axum, TLS, CLI, rate limiting)
│   ├── src/
│   │   ├── main.rs                   — server entry point, router wiring, CLI config, background tasks
│   │   ├── lib.rs                    — library root (re-exports for integration tests)
│   │   ├── route_handlers.rs         — all HTTP handlers (login, delegate, revoke, set, get, update,
│   │   │                               delete, snapshot, schema, analytics, REST get/collection)
│   │   ├── ws.rs                     — WebSocket upgrade, per-connection authenticated push
│   │   ├── server.rs                 — TLS config loader, graceful shutdown signal
│   │   └── rate_limit.rs             — per-IP sliding window rate limiter
│   ├── tests/
│   │   └── integration.rs            — integration test suite
│   └── examples/
│       ├── generate_stress_data.rs   — generates 100 000 synthetic documents
│       ├── stress_insert.rs          — bulk-inserts the dataset into a live server
│       └── stress_fetch.rs           — fires concurrent GET requests, reports latency percentiles
│
├── moltendb-wasm/                    — WASM crate (browser / Node.js bundle)
│   └── src/
│       └── lib.rs                    — wasm-bindgen entry point, OPFS-backed Db
│
├── tests/
│   └── requests.http                 — documented example requests for every endpoint
├── pkg/                              — generated WASM package (wasm-pack output)
└── assets/
    └── logo.png
```

---

## What's Next? (The Roadmap)

MoltenDB is currently in **RC Stage**. The core engine is stable, fast, and feature-rich.

### 1. Scaling & Ecosystem
- **Mobile Native Modules:** Compiling the exact same Rust core to run natively on iOS and Android (via FFI/JNI). This will bring blazing-fast, local-first embedded databases to React Native and Flutter.
- **Language Clients:** Official transport drivers for Python, Go, and Swift.
- **Data Portability:** Built-in, zero-friction utilities to export your entire database to standard JSON and CSV formats. No vendor lock-in.

### 2. Distributed Systems & Core
- **Robust Sync:** Two-way browser ↔ server delta sync with automatic conflict resolution (server-wins on `_v` collision).
- **Hardened Analytics:** The `COUNT/SUM/AVG/MIN/MAX` analytics engine exists in the codebase but is **currently under development and not ready for production use**. Expanding and rigorously testing it, accompanied by a comprehensive, interactive live demo, is a key roadmap item.

### 3. Security, Tooling & Polish
- **MoltenDB Studio (Premium):** A paid, official GUI dashboard to visually manage your databases, inspect collections, and execute queries without touching the CLI.


### What's NOT on the Roadmap (The Anti-Goals)

Keeping a project fast and lightweight means being very strict about what *not* to build. Here are a few things I have intentionally decided to leave out of MoltenDB:

- **Natural Language Queries (NLQ):** I know AI and "chat-to-query" interfaces are the hot trend right now, and it feels like every database is bolting them on.
However, MoltenDB is fundamentally designed to be lean, predictable, and exceptionally fast.
Adding NLQ or embedding a vector engine would completely destroy the lightweight footprint of the WASM build and the native binary.
While I might explore building an NLQ adapter as a completely separate middleware package down the road, it will never be baked into the core engine.
- **Heavy Data Transformations (`map`, `flat`, `flatMap`):** The query engine is highly optimized to retrieve your data (with precise field selection) as quickly as possible. 
Baking complex array manipulations or heavy map/reduce operations into the fetch pipeline adds unnecessary overhead to the core engine.
It is much faster and cleaner to let the database be a database, and handle those specific data transformations in your application layer (JavaScript/Rust) after the data is returned.

---

## License

MoltenDB is licensed under the [Business Source License 1.1](LICENSE.md).

- **Free** for personal use and organisations with annual revenue under $5 million USD.
- **Not permitted** to offer MoltenDB as a hosted/managed service (Database-as-a-Service) without a commercial license.
- **Converts to MIT** automatically 3 years after each version's release date.

For commercial licensing enquiries: maximilian.both27@outlook.com
