## Architecture

MoltenDB is structured as a **Cargo Workspace** with four independent crates. Each crate has a single, well-defined
responsibility and can be used in isolation.

```
MoltenDB/
├── moltendb-core/     — pure engine: no HTTP, no auth, no JWT, no WASM bindings
├── moltendb-wasm/     — browser adapter: wasm-bindgen glue, WorkerDb, OPFS
├── moltendb-auth/     — identity layer: JWT, Argon2, UserStore
└── moltendb-server/   — network layer: Axum, TLS, CORS, CLI config
```

### `moltendb-core` — The Engine

The heart of MoltenDB. Contains the in-memory `DashMap` store, the append-only WAL, all storage backends (disk,
encrypted, OPFS), the query evaluator (`$in`, `$gt`, joins, field projection), and all handler and validation logic
shared between the server and the WASM adapter.

**Zero knowledge of HTTP, TCP, JWT, users, or WASM bindings.** This crate compiles to:

- A native `rlib` for embedding in other Rust projects
- A `cdylib` for FFI (mobile, Tauri, etc.)

### `moltendb-wasm` — The Browser Adapter

A thin `cdylib` crate that wraps `moltendb-core` and exposes it to JavaScript via `wasm-bindgen`. Contains `WorkerDb` —
the WASM entry point used by the Web Worker — and all browser-specific glue (`web-sys`, `js-sys`, OPFS access). Built
with `wasm-pack build moltendb-wasm --target web`.

**JS initialisation** uses a named static factory (not an async constructor, which produces invalid TypeScript):

```js
// ✅ correct
const db = await WorkerDb.create("my_database");

// ❌ deprecated — do not use
const db = await new WorkerDb("my_database");
```

Keeping WASM bindings in a separate crate means `moltendb-core` and `moltendb-server` have a clean, WASM-free dependency
tree.

**Use it as an embedded database** — add it to any Rust project with no HTTP overhead:

```toml
# Cargo.toml
[dependencies]
moltendb-core = "1.0.0-rc14"
```

```rust
use moltendb_core::engine::{Db, DbConfig};

let config = DbConfig {
path: "./my_app.log".to_string(),
sync_mode: true,
..Default::default ()
};

let db = Db::open(config).await?;
db.insert_batch("users", vec![("u1".to_string(), serde_json::json!({ "name": "Alice" }))]) ?;
let user = db.get("users", "u1");
```

| Feature               | Available in `moltendb-core`? | Available in `moltendb-server`? | Why?                                                |
|:----------------------|:------------------------------|:--------------------------------|:----------------------------------------------------|
| `MOLTENDB_DB_PATH`    | No (passed via `DbConfig`)    | **Yes**                         | Engine needs a path; server provides the CLI flag.  |
| `MOLTENDB_HOST`       | **No**                        | **Yes**                         | Core has no network listener or HTTP logic.         |
| `MOLTENDB_PORT`       | **No**                        | **Yes**                         | Core has no network listener or HTTP logic.         |
| `MOLTENDB_ROOT_USER`  | **No**                        | **Yes**                         | Core doesn't handle API authentication.             |
| `MOLTENDB_JWT_SECRET` | **No**                        | **Yes**                         | Server-side token security.                         |
| `MOLTENDB_SYNC_MODE`  | No (passed via `DbConfig`)    | **Yes**                         | Controls write flush behaviour (`async` or `sync`). |
| `MOLTENDB_IN_MEMORY`  | No (passed via `DbConfig`)    | **Yes**                         | Bypasses the WAL; all data lives in RAM only.       |

> [!TIP]
> **When using the standalone `moltendb-server` binary, all flags and environment variables are available.** The server
> acts as a thin wrapper that combines the engine, authentication, and networking layers. The distinction only matters
> if
> you are using `moltendb-core` as a library in your own Rust project.

### 3. How to configure `moltendb-core` directly

If you are building a custom application and importing `moltendb-core`, you don't use environment variables or CLI flags
unless you implement them yourself. Instead, you initialize the database using the `DbConfig` struct:

```rust
use moltendb_core::engine::{Db, DbConfig};

#[tokio::main]
async fn main() {
    // Core doesn't know about MOLTENDB_PORT or MOLTENDB_ROOT_USER
    let config = DbConfig {
        path: "my_data.db".to_string(),
        sync_mode: true,
        ..Default::default()
    };

    let db = Db::open(config).await.unwrap();
    // Now you have a running database instance in your own app!
}
```

In summary: **the server flags are just a user interface for the standalone binary.** If you use the core package as a
library, you are responsible for how you want to configure it.

---

### `moltendb-auth` — The Identity Layer

Handles everything related to identity: Argon2 password hashing, JWT minting and validation (HMAC-SHA256), the
`UserStore`, and **scoped token delegation**. Depends only on `moltendb-core` — it has no knowledge of HTTP routing or
the server binary.

**Single root user.** One root user is configured at startup via `--root-user` / `--root-password`. There is no user
management API — MoltenDB is designed to work alongside your own user table. Your backend validates credentials against
your database, then calls `POST /auth/delegate` to mint a narrow-scoped JWT for the client. The root token never leaves
your backend.

**WASM excluded.** The entire crate is gated with `#![cfg(not(target_arch = "wasm32"))]` — auth is irrelevant for local
browser storage and adds no weight to the WASM bundle.

### `moltendb-server` — The Network Layer

The runnable binary. Owns Axum routing, TLS termination, CORS policy, per-IP rate limiting, HTTP body size enforcement,
and the CLI configuration (via `clap`). Parses incoming JSON requests and delegates to `moltendb-core`. Depends on both
`moltendb-core` and `moltendb-auth`.

---

> **Deployment model:** Run `moltendb-server` as a standalone HTTPS server, embed `moltendb-core` directly in your Rust
> application, or compile `moltendb-core` to WASM for browser-side local-first storage.

MoltenDB keeps the **entire dataset in RAM** (`DashMap`) — reads are pure hashmap lookups with no disk I/O. All data is
loaded into memory at startup from the snapshot + WAL delta. RAM is the hard dataset size limit.

One of MoltenDB's core features is **GraphQL like fine-grained field projection**: every query lets you specify exactly
which fields (including deeply nested ones) you want back. You never receive more data than you asked for — no
over-fetching, no under-fetching, no separate schema to maintain.

---

### Query Execution Architecture

Every `POST /get` request is routed through one of four execution paths depending on the query shape:

| Query type              | Execution path                          | Mechanism                                                                                                                                                                                                             |
|:------------------------|:----------------------------------------|:----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Key lookup (`keys`)     | DashMap direct                          | O(1) hash lookup — no scan                                                                                                                                                                                            |
| No `sort`, no `where`   | BTreeMap seq-index early-exit           | Iterates a per-collection `BTreeMap<u64, String>` (seq → key) in insertion order; breaks the moment `offset + limit` matches are collected                                                                            |
| No `sort`, with `where` | Rayon parallel scan + atomic early-exit | All CPU cores scan DashMap shards concurrently; an `AtomicUsize` counter stops threads once `offset + limit` matches are found; results sorted by `_seq` before pagination                                            |
| `sort` present          | Rayon bounded top-N (`scan_top_n_raw`)  | All cores scan in parallel; each thread maintains a bounded min-heap of size `limit`; raw byte extraction via `find_msgpack_value` — no full deserialization during scan; only the final `limit` winners are hydrated |

#### BTreeMap Seq-Index

A secondary `BTreeMap<u64, String>` (insertion sequence → document key) is maintained per collection alongside the
primary `DashMap`. It enables true O(k) ordered pagination for pure pagination queries (no `where`, no `sort`) — the
engine iterates in insertion order and breaks as soon as enough matches are found, without scanning the full collection.

```
state:     DashMap<Arc<str>, DashMap<String, Box<[u8]>>>   ← primary store (O(1) key lookup)
seq_index: DashMap<Arc<str>, RwLock<BTreeMap<u64, String>>> ← secondary index (ordered iteration)
```

- **Write cost:** O(log n) BTreeMap insert per document write (plus `RwLock` acquisition).
- **Read cost (no-where, no-sort):** O(k) where k = documents scanned to satisfy `offset + limit`.
- **Startup:** index is rebuilt from the replayed WAL state in O(n log n) — no changes to the WAL format.
- **Default order:** newest-first (`map.iter().rev()`). Pass `"order": "asc"` in the request payload to iterate
  oldest-first.

#### Deferred Hydration in `scan_top_n_raw`

Sorted queries use lazy byte extraction (`find_msgpack_value` + `read_msgpack_number`) to read only the sort field from
raw MsgPack bytes during the parallel scan phase — no `serde_json::Value` is allocated per document. Full
deserialization (`msgpack_to_value`) is deferred to the final `limit` winners only, eliminating millions of heap
allocations for large collections.

#### Bulk Delete Path (`delete_filtered`)

`POST /delete` with a `where` clause mirrors the **`No sort, with where` read path**. The predicate runs directly on the
raw MsgPack bytes (`query::evaluate_where_msgpack`) during a Rayon parallel scan — no `serde_json::Value` is decoded per
document, only the cheap `_seq` token is read for matches. This makes a bulk delete about as cheap to scan as an
equivalent unsorted `/get`, instead of paying a full `serde_json::Value` allocation for every document in the collection.

Matches are collected as `(seq, key)` pairs and **sorted by `_seq` before the `count` cap is applied**, so a
count-limited delete removes a deterministic subset rather than an arbitrary `DashMap`-iteration slice. The request's
`order` property selects the direction:

- `"asc"` (**default**) → oldest documents first (lowest `_seq`),
- `"desc"` → newest documents first (highest `_seq`).

Note the default here (`"asc"`, oldest-first) is the **opposite** of the unsorted `/get` default (`"desc"`,
newest-first): oldest-first is the natural default for pruning and retention workloads.

---

### Pagination Limitations

Each execution path has different performance characteristics for `offset`-based pagination:

| Query type              | `offset` cost                    | Explanation                                                                                                                                     |
|:------------------------|:---------------------------------|:------------------------------------------------------------------------------------------------------------------------------------------------|
| No `sort`, no `where`   | O(offset + limit)                | BTreeMap iterates in insertion order; only `offset + limit` documents are decoded before breaking.                                              |
| No `sort`, with `where` | O(N) worst case                  | Rayon atomic early-exit collects `offset + limit` matches across all cores; worst case when matching documents are clustered at the oldest end. |
| `sort` present          | O(N), heap size = offset + limit | Must find the true top `offset + limit` results globally before discarding the first `offset`. Heap grows linearly with offset depth.           |

#### Deep sorted pagination

A query with `sort` and a large `offset` (e.g. `offset: 100000, count: 10`) forces `scan_top_n_raw` to maintain a
bounded heap of 100,010 items per Rayon thread instead of 10. This increases memory pressure, heap comparison cost, and
merge overhead proportionally. The recommended alternative is **keyset pagination**: track the last field value seen
from the previous page and use it as a `where` filter, keeping the heap size equal to `count` regardless of depth.

#### `_seq`-based cursor pagination for `where` queries

For unsorted `where` queries, use the system `_seq` field as a cursor instead of `offset`. `_seq` is a monotonically
increasing `u64` stamped on every document at insert time and stored under a single-byte token key for zero-cost
extraction. Adding `"_seq": { "$lt": last_seen_seq }` to the `where` clause makes each page start exactly where the
last one ended — the Rayon atomic early-exit stops as soon as `count` matches are found, with no results to discard.
You can also query a precise insertion-order window directly:

```json
{ "where": { "_seq": { "$gt": 300000, "$lt": 300100 } } }
```

#### `$contains` / substring queries

`$ct` / `$contains` predicates on string fields always require a full collection scan — no index can skip non-matching
documents for arbitrary substring matches. The BTreeMap early-exit still fires once `offset + limit` matches are found,
so performance is best when matching documents are near the newest end of the collection and degrades toward O(N) when
they are spread throughout or clustered at the oldest end.

