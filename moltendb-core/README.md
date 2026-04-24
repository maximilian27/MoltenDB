<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-core

### 🌋 The Pure Engine Crate

**In-memory document store · Append-only WAL · Query evaluator · Analytics (🚧 WIP)**  
Zero knowledge of HTTP, auth, JWT, or WASM bindings.

[![License](https://img.shields.io/badge/license-BSL%201.1-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![crates.io](https://img.shields.io/crates/v/moltendb-core?style=flat-square)](https://crates.io/crates/moltendb-core)

</div>

---

## What is this crate?

`moltendb-core` is the heart of MoltenDB. It contains every piece of logic that is shared between the HTTP server (`moltendb-server`) and the browser WASM adapter (`moltendb-wasm`):

- **In-memory store** — `DashMap`-backed document collections, keyed by `(collection, key)`.
- **Append-only WAL** — every write is appended to a log file (`LogEntry`: INSERT, DELETE, DROP, INDEX, ENC) with an engine-level `_t` timestamp for Point-in-Time Recovery.
- **Storage backends** — `DiskStorage` (sync/async), `TieredStorage` (hot + cold log), `EncryptedStorage` (ChaCha20-Poly1305), `OpfsStorage` (WASM / browser OPFS).
- **Snapshot Versioning** — Automatically backs up old snapshots to a `/backup` folder before rotation.
- **Point-in-Time Recovery (PITR)** — Rebuild the state to any millisecond or sequence number (native only).
- **Query evaluator** — `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$nin`, `$contains`, `$or`, `$and`, field projection (include / exclude), dot-notation for nested fields, joins, sort, count, offset.
- **Analytics engine** — COUNT, SUM, AVG, MIN, MAX with optional WHERE filtering. ⚠️ **Under active development — not ready for production use.**
- **Auto-indexing** — `query_heatmap` tracks hot fields and builds indexes automatically.
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
moltendb-core = "0.6.0"
```

### Minimal example

```rust
use moltendb_core::engine::Db;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open (or create) a database log file on disk
    let db = Db::open("./my_app.log").await?;

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

let db = Db::open("./my_app.log").await?;

let payload = json!({
    "collection": "users",
    "where": { "role": "admin" },
    "fields": ["name", "role"],
    "sort": [{ "field": "name", "order": "asc" }]
});

let (status_code, result) = handlers::process_get::process_get(&db, &payload, 10 * 1024 * 1024);
println!("{} — {}", status_code, result);
```

---

## Hybrid Bitcask Storage

MoltenDB uses a **Hybrid Bitcask-inspired Storage Model**. Frequently accessed data is kept in RAM (`Hot`) as parsed JSON for sub-microsecond reads. Less frequently used data is paged out to disk (`Cold`) as byte-offsets, freeing up memory. This allows MoltenDB to handle datasets much larger than the available RAM while maintaining high performance for the active working set.

By default, any collection exceeding **50,000 documents** will automatically evict the oldest documents to the `Cold` tier (disk/OPFS). This limit is configurable when opening the database.

---

## Module overview

| Module | Responsibility |
|---|---|
| `engine` | `Db` struct, storage backends, WAL replay, operations |
| `engine::storage` | `DiskStorage`, `TieredStorage`, `EncryptedStorage`, `OpfsStorage` |
| `query` | Query condition evaluation, field projection, joins, sort, pagination |
| `analytics` | Aggregate functions: COUNT, SUM, AVG, MIN, MAX — ⚠️ under development, not ready for use |
| `handlers` | `process_get`, `process_set`, `process_update`, `process_delete`, `process_analytics` |
| `validation` | Collection / key / field name validation rules |

---

## Storage modes

| Mode | Use case |
|---|---|
| `DiskStorage` (sync) | Durable writes. Each write is flushed to disk before returning. Slower but safer for mission-critical data. |
| `DiskStorage` (async) | Blazing fast, high-throughput writes. Data is buffered and flushed in the background. Recommended for most web use-cases. |
| `TieredStorage` | 100k+ documents — separates hot and cold log files |
| `EncryptedStorage` | At-rest encryption with ChaCha20-Poly1305 |
| `OpfsStorage` | Browser WASM — Origin Private File System |

---

## Design constraints

- **No longer limited by RAM.** While MoltenDB is "Memory-First," the Hybrid Bitcask model allows it to page out documents to disk while keeping only the keys and offsets in RAM. A 10GB database can now comfortably run on a machine with 512MB of RAM.
- **No HTTP, no auth, no JWT.** This crate has zero knowledge of the network layer. It is safe to embed in any Rust application without pulling in Axum, Tokio TLS, or any auth dependency.
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
