<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-core

### 🌋 The Pure Engine Crate

**In-memory document store · Append-only WAL · Query evaluator · Analytics (🚧 WIP)**  
Zero knowledge of HTTP, auth, JWT, or WASM bindings.

[![License](https://img.shields.io/badge/license-BSL%201.1-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![crates.io](https://img.shields.io/crates/v/moltendb-core?style=flat-square)](https://crates.io/crates/moltendb-core)
[![Status](https://img.shields.io/badge/status-1.0.0--rc-blue?style=flat-square)](../CHANGELOG.md)

</div>

> [!WARNING]
> **Versions starting with `v1.0.0-rc1` are not backwards compatible with previous versions.**
> We are actively working on improving performance and stability. Please review the changelog before upgrading.

---

## What is this crate?

`moltendb-core` is the heart of MoltenDB. It contains every piece of logic that is shared between the HTTP server (`moltendb-server`) and the browser WASM adapter (`moltendb-wasm`):

- **In-memory store** — `DashMap`-backed document collections, keyed by `(collection, key)`.
- **Append-only WAL** — every write is appended to a log file (`LogEntry`: INSERT, DELETE, DROP, INDEX, ENC) with an engine-level `_t` timestamp for Point-in-Time Recovery.
- **Storage backends** — `DiskStorage` (sync/async), `EncryptedStorage` (ChaCha20-Poly1305), `OpfsStorage` (WASM / browser OPFS).
- **Snapshot Versioning** — Automatically backs up old snapshots to a `/backup` folder before rotation.
- **Point-in-Time Recovery (PITR)** — Rebuild the state to any millisecond or sequence number (native only).
- **Query evaluator** — `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$nin`, `$contains`, `$or`, `$and`, field projection (include / exclude), dot-notation for nested fields, joins, sort, count, offset.
- **Analytics engine** — COUNT, SUM, AVG, MIN, MAX with optional WHERE filtering. ⚠️ **Under active development — not ready for production use.**
- **Handler pipeline** — `process_get`, `process_set`, `process_update`, `process_delete`, `process_analytics` — the single source of truth consumed by both the server and the WASM adapter.
- **Input validation** — collection name, key, and field name rules enforced before any operation reaches the engine.

---

## Crate type

```toml
[lib]
crate-type = ["rlib"]
```

`moltendb-core` compiles to a native `rlib`. It is **not** a `cdylib` — WASM bindings live in the separate `moltendb-wasm` crate. This keeps the native dependency tree clean (no `wasm-bindgen`, no `web-sys`).

WASM-specific code (`OpfsStorage`, `Db::open_wasm`) is gated behind `#[cfg(target_arch = "wasm32")]` and only compiled when the crate is used as a dependency of `moltendb-wasm`.

---

## Add to your project

```toml
[dependencies]
moltendb-core = "1.0.0-rc2"
```

### Minimal example

```rust
use moltendb_core::engine::{Db, DbConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open (or create) a database with custom configuration
    let config = DbConfig {
        path: "./my_app.log".to_string(),
        sync_mode: true,
        ..Default::default()
    };
    let db = Db::open(config).await?;

    // Insert a document
    db.set("users", "u1", serde_json::json!({
        "name": "Alice",
        "role": "admin"
    })).await?;

    // Read it back
    let user = db.get("users", "u1").await?;
    println!("{}", user);

    Ok(())
}
```

### Using the handler pipeline (same API as the HTTP server)

```rust
use moltendb_core::{engine::Db, handlers};
use serde_json::json;

let config = DbConfig {
    path: "./my_app.log".to_string(),
    ..Default::default()
};
let db = Db::open(config).await?;

let payload = json!({
    "collection": "users",
    "where": { "role": "admin" },
    "fields": ["name", "role"],
    "sort": [{ "field": "name", "order": "asc" }]
});

let (status_code, result) = handlers::process_get::process_get(&db, &payload, 10 * 1024 * 1024);
println!("{} — {}", status_code, result);
```

> **Pagination defaults:** `count` defaults to `100` if not supplied. Values above `1000` are rejected with a `400 Bad Request` error. This applies to both `/get` and bulk `/delete` (with `where`).

---

## Storage Model

All documents are kept in RAM in a `DashMap`. On startup, the snapshot is loaded and the delta log is replayed — no cold tier, no eviction, no offset arithmetic. Compaction writes a new snapshot and resets the log.

---

## Reserved fields

Every document automatically receives the following engine-managed fields. Any field whose name starts with `_` is reserved — the handler layer rejects inserts or updates that contain such fields.

| Field | Description |
|---|---|
| `_key` | The document's own key — injected on read, never stored inside the document body |
| `_v` | Version counter, incremented on every write by the engine. Always starts at `1` for new documents. |
| `_createdAt` | ISO-8601 timestamp set once at first insert and never overwritten. Always returned in every response. |
| `_modifiedAt` | ISO-8601 timestamp updated on every write. Always returned in every response. |
| `_expiresAt` | ISO-8601 timestamp when the **collection** expires. This is a **virtual field** — never stored inside documents. Computed from the collection TTL map and injected into every response when the collection has a TTL. |

Attempting to insert or update a document containing any `_`-prefixed field (except `_v` on update) returns `400 Bad Request`.

`_key`, `_v`, `_createdAt`, and `_modifiedAt` are **always present in every response** — they are re-attached after any `fields` or `excludedFields` projection and cannot be suppressed. `_expiresAt` is also always returned when the collection has a TTL registered.

---

## TTL (Time-to-Live)

Collections can expire automatically via a **collection-level TTL**. Set it via `process_schema` (no JSON schema required) or inline on `process_set`:

```rust
// Via /schema
let payload = json!({ "collection": "cache", "ttl": 300 });
handlers::process_schema::process_schema(&db, &payload, 10 * 1024 * 1024, 1000);

// Inline on /set (shortcut)
let payload = json!({ "collection": "cache", "data": { "k": { "value": 1 } }, "ttl": 300 });
handlers::process_set::process_set(&db, &payload, 10 * 1024 * 1024, 1000);
```

**How it works:**
- The expiry clock resets to `now + ttl_secs` at the end of every insert batch — so the clock starts when the **last write commits**, not when the schema was registered.
- On expiry the **entire collection is dropped** in one O(1) `delete_collection` call.
- `_expiresAt` is a **virtual field** — never stored inside documents. It is computed from `Db::ttl_expiry` and injected into every response.
- TTL is **immutable by design** — changing the TTL requires dropping and recreating the collection.

**Eviction:**
- **Lazy** — `process_get` checks the collection expiry once per request (O(1)) and returns `404` immediately if expired.
- **Eager** (server only) — `ttl_sweep` uses an event-driven min-heap with **one entry per collection**, wakes exactly when the next collection expires, and calls `Db::delete_collection`. Zero CPU when idle.
- **WASM** — lazy eviction only (no background thread in the browser).

---

## Bulk delete with `where` filters

`process_delete` supports the same `where` clause as `process_get`, letting you delete all documents that match a filter in a single atomic operation:

```rust
use moltendb_core::{engine::Db, handlers};
use serde_json::json;

let payload = json!({
    "collection": "users",
    "where": { "role": { "$eq": "guest" } }
});

let (status, body) = handlers::process_delete::process_delete(&db, &payload, 10 * 1024 * 1024, 1000);
// body → { "status": "ok", "deleted": 42 }
```

All filter operators are supported: `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$contains`, `$in`, `$nin`, `$and`, `$or`. An optional `count` property limits how many documents are deleted (**default `100`**, max `1000`).

Internally, `Db::delete_filtered(collection, predicate, count_limit)` runs a parallel scan (rayon on native, sequential on WASM) to collect matching keys, then deletes them in a single transaction. The response always includes the count of deleted documents.

This works identically in the HTTP server, the WASM browser module, and when embedding `moltendb-core` directly in your own Rust application.

---

## Module overview

| Module | Responsibility |
|---|---|
| `engine` | `Db` struct, storage backends, WAL replay, operations |
| `engine::storage` | `DiskStorage`, `EncryptedStorage`, `OpfsStorage` |
| `query` | Query condition evaluation, field projection, joins, sort, pagination |
| `analytics` | Aggregate functions: COUNT, SUM, AVG, MIN, MAX — ⚠️ under development, not ready for use |
| `handlers` | `process_get`, `process_set`, `process_update`, `process_delete`, `process_analytics` |
| `validation` | Collection / key / field name validation rules |

---

## Storage modes

MoltenDB has three storage backends:

| Mode | `DbConfig` field | Best for |
|---|---|---|
| `AsyncDiskStorage` (default) | `sync_mode: false` | General use, max throughput |
| `SyncDiskStorage` | `sync_mode: true` | Zero data loss per write |
| `InMemoryStorage` | `in_memory: true` | Ephemeral caches, CI, tests |

### Async (default)

Single append-only log file. Writes are buffered and flushed to disk every **50 ms** — up to 50 ms of data can be lost on a hard crash. Highest write throughput.

### Sync (`sync_mode: true`)

Same single-file layout as async, but every write blocks until the OS confirms the data is on disk. **Zero data loss on crash.** Lower throughput. Use this when losing even 50 ms of writes is unacceptable (financial records, audit logs).


### In-Memory (`in_memory: true`)

Bypasses the WAL and all disk I/O entirely. All data lives exclusively in the RAM `DashMap`. Compaction is skipped. All data is lost when the process exits.

### `EncryptedStorage`

Wraps any of the above backends with ChaCha20-Poly1305 at-rest encryption. Enable via `DbConfig::encryption_key`.

### `OpfsStorage`

Browser WASM only — uses the Origin Private File System (OPFS) as the storage backend instead of the native filesystem.

---

## Snapshots, Compaction & Data Safety

Compaction is **manual-only** — trigger it explicitly via `POST /snapshot` (HTTP server) or `db.compact()` (embedded). It:

1. Writes the complete current in-memory state to a **temp snapshot file** — the live snapshot is untouched at this point.
2. **Moves the existing snapshot** to `backup/<name>.snapshot.bin.<unix_timestamp>.bak` — the old snapshot is never deleted.
3. **Atomically renames** the temp file to the live snapshot — a single OS rename, no window where neither file exists.
4. **Resets the live log to empty** — all data is already captured in the new snapshot before this happens.

### No data is lost across compactions

Each snapshot is a **full state dump**, not a diff. A document inserted in compaction 1 is present in every subsequent snapshot until it is explicitly deleted:

```
Compaction 1:  snapshot = { doc_A, doc_B }
Compaction 2:  snapshot = { doc_A, doc_B, doc_C }   ← doc_A still here
Compaction 3:  snapshot = { doc_A, doc_B, doc_C, doc_D }  ← doc_A still here
```

### The `backup/` folder

Every compaction moves the previous snapshot to `backup/` as a `.bak` file — a point-in-time copy of the full database state. These files are not loaded at startup and are not pruned automatically. Use them for manual point-in-time recovery via `Db::recover_to`.

### Startup RAM usage

At startup, `stream_into_state` reads the snapshot and applies each entry **directly into the `DashMap`** as it is read — no intermediate buffer. Peak RAM at startup is approximately **1× the snapshot file size**.

---

## Design constraints

- **Memory-First.** All documents are kept in RAM for sub-microsecond reads. Compaction + snapshot keep startup fast even for large collections.
- **No HTTP, no auth, no JWT.** This crate has zero knowledge of the network layer. It is safe to embed in any Rust application without pulling in Axum, Tokio TLS, or any auth dependency.
- **Programmatic Configuration.** Unlike `moltendb-server`, this crate does **not** parse environment variables or CLI flags. All configuration must be passed via the `DbConfig` struct.
- **Single writer, many readers.** The `DashMap` store is safe for concurrent reads. Writes are serialised through the storage backend.

---

## Testing

`moltendb-core` includes a comprehensive test suite to ensure engine reliability and query correctness.

### Unit Tests
Unit tests are located within the source files (e.g., `src/query.rs`, `src/engine/storage/mod.rs`).
```bash
cargo test -p moltendb-core --lib
```

### Integration Tests
Integration tests are located in the `tests/` directory and verify the interaction between the engine, handlers, and storage backends.
```bash
# Run all core integration tests
cargo test -p moltendb-core --test engine_tests
cargo test -p moltendb-core --test query_tests
cargo test -p moltendb-core --test hybrid_storage_tests
```

---

## Part of the MoltenDB workspace

```
MoltenDB/
├── moltendb-core/     ← you are here
├── moltendb-wasm/     — browser adapter (wasm-bindgen glue, WorkerDb, OPFS)
├── moltendb-auth/     — identity layer (JWT, Argon2, UserStore)
└── moltendb-server/   — network layer (Axum, TLS, CORS, CLI config)
```

See the [root README](../README.md) for the full architecture overview and feature list.
