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

The heart of MoltenDB. Contains the in-memory `DashMap` store, the append-only WAL, all storage backends (disk, encrypted, OPFS), the query evaluator (`$in`, `$gt`, joins, field projection), and all handler and validation logic shared between the server and the WASM adapter.

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
moltendb-core = "1.0.0-rc7"
```

```rust
use moltendb_core::engine::{Db, DbConfig};

let config = DbConfig {
    path: "./my_app.log".to_string(),
    sync_mode: true,
    ..Default::default()
};

let db = Db::open(config).await?;
db.insert_batch("users", vec![("u1".to_string(), serde_json::json!({ "name": "Alice" }))])?;
let user = db.get("users", "u1");
```

| Feature | Available in `moltendb-core`? | Available in `moltendb-server`? | Why? |
| :--- | :--- | :--- | :--- |
| `MOLTENDB_DB_PATH` | No (passed via `DbConfig`) | **Yes** | Engine needs a path; server provides the CLI flag. |
| `MOLTENDB_HOST` | **No** | **Yes** | Core has no network listener or HTTP logic. |
| `MOLTENDB_PORT` | **No** | **Yes** | Core has no network listener or HTTP logic. |
| `MOLTENDB_ROOT_USER` | **No** | **Yes** | Core doesn't handle API authentication. |
| `MOLTENDB_JWT_SECRET` | **No** | **Yes** | Server-side token security. |
| `MOLTENDB_SYNC_MODE` | No (passed via `DbConfig`) | **Yes** | Controls write flush behaviour (`async` or `sync`). |
| `MOLTENDB_IN_MEMORY` | No (passed via `DbConfig`) | **Yes** | Bypasses the WAL; all data lives in RAM only. |

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

MoltenDB keeps the **entire dataset in RAM** (`DashMap`) — reads are pure hashmap lookups with no disk I/O. All data is loaded into memory at startup from the snapshot + WAL delta. RAM is the hard dataset size limit.

One of MoltenDB's core features is **GraphQL like fine-grained field projection**: every query lets you specify exactly which fields (including deeply nested ones) you want back. You never receive more data than you asked for — no over-fetching, no under-fetching, no separate schema to maintain.

