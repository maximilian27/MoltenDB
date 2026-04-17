<div align="center">
  <img src="assets/logo.png" alt="MoltenDB Logo" width="400"/>

# MoltenDB

### 🌋 A Local-First Embedded Database in Pure Rust

**Runs in the browser (WASM + OPFS) and on the server (Rust + disk).**  
Same query engine. Same log format. Two environments.

**Request only the fields you need — like GraphQL, but over a plain JSON API.**

[![License](https://img.shields.io/badge/license-BSL%201.1-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-56%20passing-brightgreen?style=flat-square)](#testing)

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
moltendb-core = "0.2.0-beta.2"
```

```rust
use moltendb_core::engine::Db;

let db = Db::open("./my_app.log").await?;
db.set("users", "u1", serde_json::json!({ "name": "Alice" })).await?;
let user = db.get("users", "u1").await?;
```

### `moltendb-auth` — The Identity Layer

Handles everything related to identity: Argon2 password hashing, JWT minting and validation (HMAC-SHA256), and the `UserStore`. Depends only on `moltendb-core` — it has no knowledge of HTTP routing or the server binary.

**v1 is single-user only.** One admin user is configured at startup via `--admin-user` / `--admin-password`. There is no user management API — to change credentials, restart the server with updated values.

### `moltendb-server` — The Network Layer

The runnable binary. Owns Axum routing, TLS termination, CORS policy, per-IP rate limiting, HTTP body size enforcement, and the CLI configuration (via `clap`). Parses incoming JSON requests and delegates to `moltendb-core`. Depends on both `moltendb-core` and `moltendb-auth`.

---

> **Deployment model:** Run `moltendb-server` as a standalone HTTPS server, embed `moltendb-core` directly in your Rust application, or compile `moltendb-core` to WASM for browser-side local-first storage.

All data is kept in RAM for the lifetime of the server process — there is no eviction, TTL, or page cache. Once a document is loaded it stays in memory until explicitly deleted or the process exits. This means **RAM is the hard limit on dataset size**. A 100 000-document collection of typical JSON objects occupies roughly 100–200 MB of RAM. The tiered storage mode separates hot and cold logs on disk but both are still fully loaded into the same in-memory `DashMap` on startup — tiered storage improves write throughput, not memory usage.

One of MoltenDB's core features is **GraphQL-style field selection**: every query lets you specify exactly which fields (including deeply nested ones) you want back. You never receive more data than you asked for — no over-fetching, no under-fetching, no separate schema to maintain.

---

## What Actually Works Today

### ✅ Browser (WASM + OPFS)
- Full document store running inside a Web Worker — zero main-thread blocking
- Data persists across page reloads using the Origin Private File System (OPFS)
- Automatic log compaction: count-based (every 500 inserts) and size-based (> 5 MB)
- **[`@moltendb-web/core` on NPM](https://www.npmjs.com/package/@moltendb-web/core)** — bundles the WASM engine, Web Worker, and main-thread client into a single publishable artifact
- **[`@moltendb-web/query` on NPM](https://www.npmjs.com/package/@moltendb-web/query)** — type-safe, chainable query builder (CJS + ESM + `.d.ts`)
- **[`@moltendb-web/angular` on NPM](https://www.npmjs.com/package/@moltendb-web/angular)** — official Angular wrapper for seamless integration
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
- Document versioning: every document automatically gets `_v`, `createdAt`, `modifiedAt`
- Conflict resolution: incoming writes with `_v ≤ stored _v` are silently skipped (server wins)
- Inline reference embedding (`extends`): embed data from another collection at insert time

### ✅ Security
- Passwords hashed with bcrypt / argon2
- JWT tokens signed with HMAC-SHA256, 24-hour expiry
- Credentials loaded from environment variables at startup (no hardcoded defaults in production)
- **Single-user mode only (v1):** MoltenDB supports exactly one admin user. There is no user management API — to change credentials, restart the server with updated `--admin-user` / `--admin-password` values.
- Input validation: collection names, key names, field names, JSON depth (max 32), payload size (max 10 MB), batch size (max 1000 keys)
- Security headers on every response: `X-Content-Type-Options`, `X-Frame-Options`, `HSTS`, `CSP`, etc.
- Graceful shutdown: drains in-flight requests (up to 30 s), then awaits the async writer task to fully flush all buffered log entries before exit

### ✅ Developer Tooling
- **Interactive WASM Browser Demo** — A complete, live environment to test raw JSON queries and the chainable builder directly in your browser.
  - [Run Live on StackBlitz](https://stackblitz.com/~/github.com/maximilian27/moltendb-wasm-demo) (Zero setup required)
  - [View WASM Demo Source Code (GitHub)](https://github.com/maximilian27/moltendb-wasm-demo)
- **[Server Integration Test Suite (GitHub)](https://github.com/maximilian27/moltendb-server-test)** — A browser-based testing environment to exercise the HTTP API and WebSocket endpoint against a live server using the TypeScript client. Includes an interactive Server Query Builder, a WebSocket tester, and a collection fetcher.
- **57+ documented example requests** in `tests/requests.http`
- **56 integration tests** covering all query features, versioning, persistence, compaction, concurrency, and analytics.
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
moltendb-core = "0.2.0-beta.2"
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
wasm-pack build moltendb-core --target web
```

### Run the server

```bash
# Set credentials (REQUIRED)
export MOLTENDB_ADMIN_USER=myuser
export MOLTENDB_ADMIN_PASSWORD=str0ng-p4ssw0rd
export JWT_SECRET=another-strong-secret

# Run the server binary
cargo run --release -p moltendb-server

# Or with CLI flags (equivalent)
cargo run --release -p moltendb-server -- \
  --admin-user myuser \
  --admin-password str0ng-p4ssw0rd \
  --jwt-secret another-strong-secret \
  --encryption-key my-encryption-password \
  --port 1538

# Verbose debug logging (optimizer, indexing, compaction details)
cargo run --release -p moltendb-server -- --debug
```

Run `cargo run -p moltendb-server -- --help` to see all available flags.


### Quick Test with `requests.http`

If you want to quickly test the functionality with the requests.http file, you should start the server with the following credentials (via CLI flags or environment variables): \
  **--admin-user `admin`**\
  **--admin-password `admin123`**\
Make sure to login first and then replace the token in the requests.http file with the one you get from the login response.

---

## HTTP API

All endpoints except `/login` require an `Authorization: Bearer <token>` header.  
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

Returns `{ "token": "<jwt>" }`.

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

| Property | Type | Description |
|---|---|---|
| `collection` | string | **Required.** The collection to query. |
| `keys` | string \| string[] | Fetch one or more documents by key. Returns the document directly for a single string; returns an array for an array of keys. |
| `where` | object | Filter documents. All conditions at the top level are ANDed together. |
| `fields` | string[] | **GraphQL-style field selection.** Return only these fields. Dot-notation selects nested fields. Mutually exclusive with `excludedFields`. |
| `excludedFields` | string[] | Return everything *except* these fields. Mutually exclusive with `fields`. |
| `joins` | object[] | Cross-collection joins. Each element is `{ "alias": "<name>", "from": "<collection>", "on": "<foreign_key_field>", "fields": [...] }`. |
| `sort` | object[] | Sort results. Each spec is `{ "field": "<name>", "order": "asc" \| "desc" }`. Multiple specs applied in priority order. |
| `count` | number | Maximum number of results to return (applied after filtering and sorting). |
| `offset` | number | Number of results to skip (for stable pagination, applied after sorting). |

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
    { "alias": "ram",    "from": "memory",  "on": "memory_id",  "fields": ["capacity_gb", "type"] },
    { "alias": "screen", "from": "display", "on": "display_id", "fields": ["size_inch", "panel", "refresh_hz"] }
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
  .joins([{ alias: 'screen', from: 'display', on: 'display_id', fields: ['panel', 'refresh_hz'] }])
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

## Configuration Reference

All options can be set via CLI flags or environment variables. CLI flags take priority.

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--port` | `PORT` | `1538` | TCP port |
| `--db-path` | `DB_PATH` | `my_database.log` | Log file path |
| `--cert` | `TLS_CERT` | `cert.pem` | TLS certificate |
| `--key` | `TLS_KEY` | `key.pem` | TLS private key |
| `--encryption-key` | `ENCRYPTION_KEY` | built-in default ⚠️ | At-rest encryption password |
| `--disable-encryption` | `DISABLE_ENCRYPTION` | `false` | Store data as plain JSON |
| `--write-mode` | `WRITE_MODE` | `async` | `async` or `sync` |
| `--storage-mode` | `STORAGE_MODE` | `standard` | `standard` or `tiered` |
| `--rate-limit-requests` | `RATE_LIMIT_REQUESTS` | `100` | Max requests per IP per window |
| `--rate-limit-window` | `RATE_LIMIT_WINDOW_SECS` | `60` | Window size in seconds |
| `--jwt-secret` | `JWT_SECRET` | **REQUIRED** 🔥 | JWT signing secret |
| `--admin-user` | `MOLTENDB_ADMIN_USER` | **REQUIRED** 🔥 | Admin username |
| `--admin-password` | `MOLTENDB_ADMIN_PASSWORD` | **REQUIRED** 🔥 | Admin password |
| `--cors-origin` | `CORS_ORIGIN` | `*` ⚠️ | Allowed CORS origin(s). Use `*` for dev only; set to your frontend URL in production (comma-separated for multiple) |
| `--max-body-size` | `MAX_BODY_SIZE` | `10485760` (10 MB) | Maximum request body size in bytes. Requests exceeding this are rejected at the HTTP layer. |
| `--debug` | `DEBUG` | `false` | Enable verbose debug logging |

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
│       ├── worker.rs                 — WASM entry point (cfg-gated)
│       ├── analytics.rs              — COUNT/SUM/AVG/MIN/MAX analytics engine
│       ├── engine/
│       │   ├── mod.rs                — Db struct, open() / open_wasm()
│       │   ├── operations.rs         — insert_batch, update, delete, versioning, WS broadcast
│       │   ├── indexing.rs           — auto-indexing, query heatmap
│       │   ├── types.rs              — LogEntry, DbError
│       │   └── storage/
│       │       ├── mod.rs            — StorageBackend trait, startup replay
│       │       ├── disk.rs           — AsyncDiskStorage, SyncDiskStorage, snapshots
│       │       ├── encrypted.rs      — XChaCha20-Poly1305 encryption wrapper
│       │       ├── tiered.rs         — TieredStorage, MmapLogReader
│       │       └── wasm.rs           — OpfsStorage (browser OPFS backend)
│       └── handlers/
│           ├── mod.rs
│           ├── process_get.rs        — GET handler (query, field selection, joins, pagination)
│           ├── process_set.rs        — SET handler (insert/upsert, extends resolution)
│           ├── process_update.rs     — UPDATE handler (partial merge, $unset)
│           ├── process_delete.rs     — DELETE handler (single, batch, drop)
│           └── process_analytics.rs  — analytics handler (reserved for future use)
│
├── moltendb-auth/                    — identity crate (JWT, Argon2, UserStore)
│   └── src/
│       └── lib.rs                    — JWT minting/validation, password hashing, UserStore
│
├── moltendb-server/                  — network crate (Axum, TLS, CLI, rate limiting)
│   ├── src/
│   │   ├── main.rs                   — server entry point, router, middleware, CLI config
│   │   ├── lib.rs                    — library root (re-exports for integration tests)
│   │   ├── validation.rs             — input validation (collection names, depth, size)
│   │   └── rate_limit.rs             — per-IP sliding window rate limiter
│   ├── tests/
│   │   └── integration.rs            — 56 integration tests
│   └── examples/
│       ├── generate_stress_data.rs   — generates 100 000 synthetic documents
│       ├── stress_insert.rs          — bulk-inserts the dataset into a live server
│       └── stress_fetch.rs           — fires concurrent GET requests, reports latency percentiles
│
├── tests/
│   └── requests.http                 — 57+ documented example requests for every endpoint
├── pkg/                              — generated WASM package (wasm-pack output)
└── assets/
    └── logo.png
```

---

## What's Next? (The Roadmap)

MoltenDB is currently in **Beta**. The core engine is stable, fast, and feature-rich, but the road to `v1.0` is going to be heavily driven by following roadmap and community feedback.

Because I am a solo developer and I don't make any money from this project (yet?), my personal life comes first. I am moving at a sustainable pace to ensure the architecture stays clean and I don't burn out. Instead of locking into a rigid feature timeline, development is focused on three major architectural themes. **If you need a specific feature to adopt MoltenDB, please open a GitHub Issue or vote on existing ones so it gets prioritized!**

### 1. Scaling & Ecosystem
- **Mobile Native Modules:** Compiling the exact same Rust core to run natively on iOS and Android (via FFI/JNI). This will bring blazing-fast, local-first embedded databases to React Native and Flutter.
- **Language Clients:** Official transport drivers for Python, Go, and Swift.
- **Data Portability:** Built-in, zero-friction utilities to export your entire database to standard JSON and CSV formats. No vendor lock-in.

### 2. Distributed Systems & Core
- **Robust Sync:** Two-way browser ↔ server delta sync with automatic conflict resolution (server-wins on `_v` collision).
- **Transactions:** ACID multi-key writes with optimistic locking (`BEGIN`, `COMMIT`, `ROLLBACK`).
- **Hardened Analytics:** Expanding and rigorously testing the `COUNT/SUM/AVG` analytics engine, accompanied by a comprehensive, interactive live demo.

### 3. Security, Tooling & Polish
- **Schema Validation:** Optional, opt-in per-collection type constraints (enforcing strings, numbers, required fields).
- **Granular ACLs:** User management and role-based access control for individual collections.
- **MoltenDB Studio (Premium):** A paid, official GUI dashboard to visually manage your databases, inspect collections, and execute queries without touching the CLI.
- **Comprehensive Changelog:** Establishing a clear, detailed changelog so the community can easily track new features, API adjustments, and performance improvements release by release.
- **A "Professional" Logo:** I know the current logo isn't exactly boring and corporate enough for an enterprise database, but I wanted the Beta release to have a bit of personality!
As we approach `v1.0`, MoltenDB will get a clean, professional brand identity.


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
