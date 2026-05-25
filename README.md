<div align="center">
  <img src="assets/logo.png" alt="MoltenDB Logo" width="400"/>

# MoltenDB

### 🌋 A portable database engine with identical execution semantics across runtime environments.

**Runs natively on the server (Rust binary), in the browser (WASM + OPFS), and purely in memory.**  
Same query engine. Same storage semantics. Different execution backends.

**Server binary under 6MB. WASM engine fits on a floppy disk.**

[![License](https://img.shields.io/badge/license-Apache--2.0%20%2F%20Elastic--2.0-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-88%20passing-brightgreen?style=flat-square)](./docs/api-reference.md#testing)
[![Status](https://img.shields.io/badge/status-1.0.0--rc-blue?style=flat-square)](CHANGELOG.md)

**🚀 Release Candidate (v1.0.0-rc)** — The API is stable. Suitable for early production use. Minor breaking changes may occur before the final 1.0.0 release.

> 🌐 **Building for the browser?** The WebAssembly engine, TypeScript client, and React/Angular adapters live in the [moltendb-web](https://github.com/moltendb/moltendb-web) repository **(MIT Licensed)**.

[⚡ Live Angular Demo](https://moltendb-angular.maximilian-both27.workers.dev/laptops) · [⚡ Live WASM Demo on StackBlitz](https://stackblitz.com/~/github.com/maximilian27/moltendb-wasm-demo)

</div>

---

## What is MoltenDB?

MoltenDB is a portable document engine written in Rust. It provides a consistent query model and storage format that can run natively on a server, in the browser (WASM + OPFS), or purely in memory. Everything else—HTTP, auth, WebSockets—is an optional adapter layer built around the same core engine.

---


[➡️ Read the full Architecture Deep Dive](docs/architecture.md)

## What Actually Works Today

### 1️⃣ Layer 1: Core Engine (Identity)
- **Fine-grained field projection (GraphQL-style selection)** — request only the fields you need using `fields` (include) or `excludedFields` (exclude). Dot-notation works at any depth: `"specs.display.features.refresh_rate"` returns only that one nested value, not the whole document.
- **In-memory store:** the entire dataset lives in RAM (`DashMap`) — reads are pure hashmap lookups with no disk I/O; RAM is the hard dataset size limit
- `WHERE` clause with: `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$contains` / `$ct` (strings and arrays), `$in` / `$oneOf`, `$nin` / `$notIn` — all string comparisons are **case-insensitive**
- Field projection (`fields`) and field exclusion (`excludedFields`) — mutually exclusive, validated before any data is read
- Pagination: `count` (limit) and `offset`. Note: While `offset` is fully supported, **Cursor Pagination** using the system `_seq` field is the recommended pagination pattern for large datasets to avoid memory/performance overhead.
- Cross-collection joins with dot-notation foreign keys
- **Snapshot Exports:** Atomic, non-blocking binary snapshots for fast recovery and off-site backups.
- **JSON Schema Validation:** High-speed consistency enforcement on a per-collection basis.
- **Optimistic Concurrency Control:** Improved version conflict detection and `409 Conflict` reporting.
- **Document versioning:** every document automatically gets `_v`, `createdAt`, `modifiedAt`
- **Atomic Batch Transactions:** WAL transaction markers (`TX_BEGIN`/`TX_COMMIT`) prevent partial write failures.
- Conflict resolution: incoming writes with stale `_v` return a `409 Conflict` error.
- Inline reference embedding (`extends`): embed data from another collection at insert time

### 2️⃣ Layer 2: Storage & Runtime Adapters

**Browser (WASM + OPFS)**
- Full document store running inside a Web Worker — zero main-thread blocking
- Data persists across page reloads using the Origin Private File System (OPFS)
- Manual compaction via `POST /snapshot` — no surprise I/O spikes during writes
- **Point-in-Time Recovery Ready:** Every write in the browser now includes a `_t` timestamp. While the recovery tool runs natively, browser logs can be exported and recovered to any millisecond using the native CLI.

**Native Disk & Server (Rust binary)**
- Two write modes: async (50 ms flush, high throughput) and sync (flush-on-write, zero data loss)
- Binary snapshots for fast startup (snapshot + delta replay, not full log replay)
- **Point-in-Time Recovery (PITR):** Recover the database to any millisecond or log sequence number using the `recover` CLI command.
- **Snapshot Versioning:** Historical snapshots are automatically moved to a `/backup` folder with Unix timestamps.
- **Manual Snapshots:** Trigger a snapshot on demand via the `POST /snapshot` endpoint.
- **Post-Backup Hook:** Automatically execute custom shell commands (e.g., S3 upload, Slack notify) after every successful snapshot.

### 3️⃣ Layer 3: Optional Services & Network
- HTTPS-only server with TLS (cert + key required)
- JWT authentication (`POST /login` → bearer token)
- Per-IP sliding-window rate limiting
- At-rest encryption with XChaCha20-Poly1305 (on by default, key from `--encryption-key`)
- WebSocket endpoint (`/ws`) for real-time push notifications — subscribe and receive change events on every write
- Passwords hashed with Argon2id
- JWT tokens signed with HMAC-SHA256; root tokens carry `*:*:*` scope (24-hour expiry)
- **Scoped token delegation:** root user mints narrow-permission JWTs for clients via `POST /auth/delegate`. Scope format: `action:collection:document_key` (e.g. `read:laptops:lp1`, `write:users:*`, `read:*:*`). Every endpoint enforces scopes — tokens missing the required scope receive `403 Forbidden`.
- **Document-level access control:** a token with `read:laptops:lp1` can only read that one document. `POST /get` without a key filter automatically returns only the documents the token is permitted to see.
- **Only the root user can mint `*:*:*` (admin) tokens** — non-root admin tokens cannot escalate their own privileges.
- **Token revocation (JTI blacklist):** every JWT carries a unique `jti` (UUID). Compromised or leaked tokens can be immediately invalidated via `DELETE /auth/tokens/:jti` (admin-only) before their TTL expires. The revocation store is persisted to `<db-path>.revocations.json` and reloaded on server restart — revocations survive restarts.
- Credentials loaded from environment variables at startup (no hardcoded defaults in production)
- **Single root user:** MoltenDB supports exactly one root user. Your own user table handles the rest — MoltenDB acts as a stateless delegation gateway, not an identity provider. Note that while the in-memory user store is ephemeral, the **token revocation list is persisted** to `<db-path>.revocations.json` and reloaded on every server restart — a revoked JWT remains revoked even after a crash or restart.
- Input validation: collection names, key names, field names, JSON depth (max 32), payload size (max 10 MB), batch size (max 1000 keys)
- Security headers on every response: `X-Content-Type-Options`, `X-Frame-Options`, `HSTS`, `CSP`, etc.
- Graceful shutdown: drains in-flight requests (up to 30 s), then awaits the async writer task to fully flush all buffered log entries before exit

### 🧩 Ecosystem & Tooling
- **[`@moltendb-web/core` on NPM](https://www.npmjs.com/package/@moltendb-web/core)** — bundles the WASM engine, Web Worker, and main-thread client into a single publishable artifact
- **[`@moltendb-web/query` on NPM](https://www.npmjs.com/package/@moltendb-web/query)** — type-safe, chainable query builder (CJS + ESM + `.d.ts`)
- **[`@moltendb-web/angular` on NPM](https://www.npmjs.com/package/@moltendb-web/angular)** — official Angular wrapper for seamless integration
- **[`@moltendb-web/react` on NPM](https://www.npmjs.com/package/@moltendb-web/react)** — official React wrapper for seamless integration
- **Interactive WASM Browser Demo** — A complete, live environment to test raw JSON queries and the chainable builder directly in your browser.
  - [Run Live on StackBlitz](https://stackblitz.com/~/github.com/maximilian27/moltendb-wasm-demo) (Zero setup required)
  - [View WASM Demo Source Code (GitHub)](https://github.com/maximilian27/moltendb-wasm-demo)
- **[Server Integration Test Suite (GitHub)](https://github.com/maximilian27/moltendb-server-test)** — A browser-based testing environment to exercise the HTTP API and WebSocket endpoint against a live server using the TypeScript client. Includes an interactive Server Query Builder, a WebSocket tester, and a collection fetcher.
- **57+ documented example requests** in `tests/requests.http`
- **80+ integration tests** covering all query features, versioning, persistence, compaction, concurrency, and schema validation.
- **Rust stress-test examples** (`examples/`) — generate 100 000 synthetic documents, bulk-insert via HTTP, and run 10 000–100 000 concurrent fetch requests with a full latency percentile report.

### What's NOT on the Roadmap (The Anti-Goals)

Keeping a project fast and lightweight means being very strict about what *not* to build. Here are a few things I have intentionally decided to leave out of MoltenDB:

- **Natural Language Queries (NLQ):** No “chat-to-query” interface in core.
  MoltenDB is fundamentally designed to be lean, predictable, and exceptionally fast.
  Adding NLQ or embedding a vector engine would completely destroy the lightweight footprint of the WASM build and the native binary.
  While I might explore building an NLQ adapter as a completely separate middleware package down the road, it will never be baked into the core engine.
- **Heavy Data Transformations (`map`, `flat`, `flatMap`):** The query engine is highly optimized to retrieve your data (with precise field selection) as quickly as possible.
  Baking complex array manipulations or heavy map/reduce operations into the fetch pipeline adds unnecessary overhead to the core engine.
  It is much faster and cleaner to let the database be a database, and handle those specific data transformations in your application layer (JavaScript/Rust) after the data is returned.


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
moltendb-core = "1.0.0-rc5"
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

## License

MoltenDB is distributed under a dual-license system:

- **Core & WASM Crates:** Licensed under the [Apache License, Version 2.0](LICENSE.md).
- **Server & Auth Crates:** Licensed under the [Elastic License 2.0](LICENSE.md).

Under the Elastic License 2.0:
- You may use, copy, modify, and redistribute the software for free.
- **Not permitted:** You may not provide the software to third parties as a hosted or managed service.

For commercial licensing or custom enterprise agreements: [admin@moltendb.dev](mailto:admin@moltendb.dev)

[➡️ View the full API Reference & Documentation](docs/api-reference.md)
