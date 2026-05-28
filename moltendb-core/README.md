<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-core

### 🌋 The Engine Kernel

**The shared core that powers every MoltenDB runtime.**  
Zero knowledge of HTTP, auth, JWT, or WASM bindings.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square)](../LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![crates.io](https://img.shields.io/crates/v/moltendb-core?style=flat-square)](https://crates.io/crates/moltendb-core)
[![Status](https://img.shields.io/badge/status-1.0.0--rc4-blue?style=flat-square)](../CHANGELOG.md)

</div>

> [!WARNING]
> **Versions starting with `v1.0.0-rc1` are not backwards compatible with previous versions.**
> Review the [changelog](../CHANGELOG.md) before upgrading.

---

## What is this crate?

`moltendb-core` is the engine kernel of MoltenDB — the single crate shared by the HTTP server (`moltendb-server`) and the browser WASM adapter (`moltendb-wasm`). It has no knowledge of the network layer, authentication, or WASM bindings. Everything above this layer is an optional adapter.

### Layer 1: Query Engine
- **Query evaluator** — `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$nin`, `$contains`, `$or`, `$and`
- **Fine-grained field projection** — include (`fields`) or exclude (`excludedFields`) at any dot-notation depth
- **Joins, sort, pagination** — cross-collection joins, multi-field sort, `count` / `offset`. (Note: Cursor pagination using the system `_seq` field is recommended over `offset` for optimal performance and memory on deep datasets).
- **Input validation** — collection name, key, and field name rules enforced before any operation reaches the engine

### Layer 2: Storage & Runtime Adapters
- **In-memory store** — `DashMap`-backed document collections, keyed by `(collection, key)`
- **Append-only WAL** — every write is appended as a `LogEntry` (INSERT, DELETE, DROP, INDEX, ENC) with a `_t` timestamp
- **Storage backends** — `AsyncDiskStorage` (default), `SyncDiskStorage`, `InMemoryStorage`, `EncryptedStorage` (ChaCha20-Poly1305), `OpfsStorage` (WASM / browser OPFS)
- **Snapshot Versioning** — old snapshots are moved to `/backup` before rotation; no data is ever deleted
- **Point-in-Time Recovery (PITR)** — rebuild state to any millisecond or sequence number (native only)

### Handler Pipeline
- `process_get`, `process_set`, `process_update`, `process_delete`, `process_analytics` — the single source of truth consumed by both the server and the WASM adapter

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
moltendb-core = "1.0.0-rc7"
```

### Minimal example

```rust
use moltendb_core::engine::{Db, DbConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DbConfig {
        path: "./my_app.log".to_string(),
        sync_mode: true,
        ..Default::default()
    };
    let db = Db::open(config).await?;

    db.set("users", "u1", serde_json::json!({
        "name": "Alice",
        "role": "admin"
    })).await?;

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
>
> 💡 **Recommended Pagination Pattern:** Although `offset` is fully supported, it is **highly recommended** to use **Cursor Pagination** for large or deep datasets. You can query and track the system-managed `_seq` property (monotonically increasing document sequence number) as a cursor (e.g. `where: { "_seq": { "$gt": last_seen_seq } }`) to achieve $O(1)$ query times and zero memory overhead under deep pagination.

---

## Storage Model

All documents are kept in RAM in a `DashMap`. On startup, the snapshot is loaded and the delta log is replayed — no cold tier, no eviction, no offset arithmetic. Compaction writes a new snapshot and resets the log.

---

## Storage modes

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

## Reserved fields

Every document automatically receives the following engine-managed fields. Any field whose name starts with `_` is reserved — the handler layer rejects inserts or updates that contain such fields.

| Field | Description |
|---|---|
| `_key` | The document's own key — injected on read, never stored inside the document body |
| `_v` | Version counter, incremented on every write by the engine. Always starts at `1` for new documents. |
| `_seq` | Monotonic insertion sequence number — strictly increasing within a collection. Assigned at first insert and preserved on overwrites. Used for FIFO eviction when `maxSize` is set, and highly recommended as a cursor for Cursor Pagination (O(1) deep pagination). **Opt-in** — only returned when explicitly listed in `fields`. |
| `_createdAt` | ISO-8601 timestamp set once at first insert and never overwritten. **Opt-in** — only returned when explicitly listed in `fields`. |
| `_modifiedAt` | ISO-8601 timestamp updated on every write. **Opt-in** — only returned when explicitly listed in `fields`. |
| `_expiresAt` | ISO-8601 timestamp when the **collection** expires. This is a **virtual field** — never stored inside documents. **Opt-in** — only returned when explicitly listed in `fields` (only relevant for TTL collections). |

**`_key` and `_v` are always present in every response** — they are protocol primitives and cannot be suppressed by `fields` or `excludedFields`.

`_seq`, `_createdAt`, `_modifiedAt`, and `_expiresAt` are **opt-in** — they are never returned unless explicitly listed in a `fields` projection.

---

## TTL (Time-to-Live)

MoltenDB supports **collection-level TTL** — an entire collection expires and is dropped automatically after a configurable idle period.

```rust
// Via /schema
let payload = json!({ "collection": "cache", "ttl": 300 });
handlers::process_schema::process_schema(&db, &payload, 10 * 1024 * 1024, 1000);

// Inline on /set (registers TTL and inserts in one call)
let payload = json!({ "collection": "cache", "data": { "k": { "value": 1 } }, "ttl": 300 });
handlers::process_set::process_set(&db, &payload, 10 * 1024 * 1024, 1000);
```

- The expiry clock resets to `now + ttl_secs` at the end of **every insert batch** — idle time since last write, not since schema registration.
- On expiry the **entire collection is dropped** in one O(1) `Db::delete_collection` call.
- `_expiresAt` is a **virtual field** — computed from `Db::ttl_expiry` and injected into every response when the collection has a TTL.
- TTL is **immutable by design** — once set, it cannot be changed without dropping and recreating the collection.
- `/update` calls do **not** reset the expiry clock — only `/set` (insert) does.

> **Design decision — sliding-window expiry:** The TTL clock resets on every insert, not on every access. This makes MoltenDB TTL ideal for **ephemeral caches, analytics buffers, and temporary working sets**. It is **not** designed for per-document expiry (OTPs, session tokens) — for those, store your own `expires_at` field and use `delete_filtered` with a time-based predicate.

**Eviction:**
- **Lazy** — `process_get` checks the collection expiry once per request (O(1)) and returns `404` immediately if expired.
- **Eager** (server only) — `ttl_sweep` uses an event-driven min-heap, wakes exactly when the next collection expires, and calls `Db::delete_collection`. Zero CPU when idle.
- **WASM** — lazy eviction only (no background thread in the browser).

---

## Capped Collections (`maxSize`)

Collections can be capped to a maximum document count. When the collection exceeds `maxSize` after an insert batch, the **oldest documents** (lowest `_seq`) are evicted automatically.

```rust
// Via /schema
let payload = json!({ "collection": "recent_events", "maxSize": 100 });
handlers::process_schema::process_schema(&db, &payload, 10 * 1024 * 1024, 1000);

// Inline on /set
let payload = json!({ "collection": "top5", "maxSize": 5, "data": { "s1": { "score": 9800 } } });
handlers::process_set::process_set(&db, &payload, 10 * 1024 * 1024, 1000);
```

- Eviction is **FIFO** — the document with the lowest `_seq` is always evicted first.
- Overwrites preserve the original `_seq`, so a document's position in the eviction queue is fixed at first insert.
- `maxSize` can be combined with `ttl` on the same collection.
- Works identically on native, WASM, and embedded usage.

---

## Bulk delete with `where` filters

`process_delete` supports the same `where` clause as `process_get`, letting you delete all matching documents in a single atomic operation:

```rust
let payload = json!({
    "collection": "users",
    "where": { "role": { "$eq": "guest" } }
});

let (status, body) = handlers::process_delete::process_delete(&db, &payload, 10 * 1024 * 1024, 1000);
// body → { "status": "ok", "deleted": 42 }
```

All filter operators are supported. An optional `count` property limits how many documents are deleted (**default `100`**, max `1000`).

---

## Collection Stats

`process_stats` returns document counts per collection. TTL-aware: expired collections report `count: 0` and `expired: true`.

```rust
// All collections
let (code, body) = process_stats(&db, &json!({}));

// Single collection
let (code, body) = process_stats(&db, &json!({ "collection": "laptops" }));
```

---

## Snapshots, Compaction & Data Safety

Compaction is **manual-only** — trigger it explicitly via `POST /snapshot` (HTTP server) or `db.compact()` (embedded). It:

1. Writes the complete current in-memory state to a **temp snapshot file**.
2. **Moves the existing snapshot** to `backup/<name>.snapshot.bin.<unix_timestamp>.bak`.
3. **Atomically renames** the temp file to the live snapshot.
4. **Resets the live log to empty**.

Each snapshot is a **full state dump**, not a diff. A document inserted in compaction 1 is present in every subsequent snapshot until explicitly deleted.

At startup, `stream_into_state` reads the snapshot and applies each entry **directly into the `DashMap`** as it is read — no intermediate buffer. Peak RAM at startup is approximately **1× the snapshot file size**.

---

## Module overview

| Module | Responsibility |
|---|---|
| `engine` | `Db` struct, storage backends, WAL replay, operations |
| `engine::storage` | `DiskStorage`, `EncryptedStorage`, `OpfsStorage` |
| `query` | Query condition evaluation, field projection, joins, sort, pagination |
| `handlers` | `process_get`, `process_set`, `process_update`, `process_delete`, `process_analytics` |
| `validation` | Collection / key / field name validation rules |

---

## Design constraints

- **Memory-First.** All documents are kept in RAM for sub-microsecond reads.
- **No HTTP, no auth, no JWT.** Safe to embed in any Rust application without pulling in Axum, Tokio TLS, or any auth dependency.
- **Programmatic Configuration.** All configuration is passed via the `DbConfig` struct — no environment variables, no CLI flags.
- **Single writer, many readers.** The `DashMap` store is safe for concurrent reads. Writes are serialised through the storage backend.

---

## Testing

```bash
# Unit tests
cargo test -p moltendb-core --lib

# Integration tests
cargo test -p moltendb-core --test engine_tests
cargo test -p moltendb-core --test query_tests
cargo test -p moltendb-core --test hybrid_storage_tests
```

---

## Part of the MoltenDB workspace

```
MoltenDB/
├── moltendb-core/     ← you are here (engine kernel)
├── moltendb-wasm/     — browser adapter (wasm-bindgen glue, WorkerDb, OPFS)
├── moltendb-auth/     — identity layer (JWT, Argon2, UserStore)
└── moltendb-server/   — network layer (Axum, TLS, CORS, CLI config)
```

See the [root README](../README.md) for the full architecture overview and feature list.
