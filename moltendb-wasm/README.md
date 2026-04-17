<div align="center">
  <img src="../assets/logo.png" alt="MoltenDB Logo" width="300"/>

# moltendb-wasm

### 🌐 The Browser Adapter Crate

**wasm-bindgen glue · WorkerDb · OPFS storage · Web Worker entry point**  
Exposes the `moltendb-core` engine to JavaScript. Zero server-side code.

[![License](https://img.shields.io/badge/license-BSL%201.1-blue?style=flat-square)](LICENSE.md)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square)](https://www.rust-lang.org)
[![crates.io](https://img.shields.io/crates/v/moltendb-wasm?style=flat-square)](https://crates.io/crates/moltendb-wasm)

</div>

---

## What is this crate?

`moltendb-wasm` is the thin browser adapter that wraps `moltendb-core` and exposes it to JavaScript via `wasm-bindgen`. It contains:

- **`WorkerDb`** — the WASM-exported struct used by the JavaScript Web Worker. Wraps the core `Db` engine and routes `postMessage` actions (`get`, `set`, `update`, `delete`, `compact`, `get_size`) to the correct handler.
- **OPFS storage** — data is persisted in the browser's Origin Private File System. Each unique `db_name` is a separate OPFS file. Data survives page reloads and browser restarts.
- **Auto-compaction** — triggered automatically after every 500 writes or when the OPFS file exceeds 5 MB. Can also be triggered manually via `compact`.
- **Real-time events** — `subscribe(callback)` taps into the same change-feed channel used by the server's WebSocket endpoint. The callback receives a JSON string for every mutation (`change`, `delete`, `drop`).
- **`wasm-bindgen` glue** — `wasm-pack build moltendb-wasm --target web` generates `moltendb_core.js`, `moltendb_core_bg.wasm`, and TypeScript declarations, ready to be bundled into `@moltendb-web/core`.

---

## Crate type

```toml
[lib]
name = "moltendb_core"   # keeps generated filenames identical to the previous build
crate-type = ["cdylib", "rlib"]
```

The `[lib] name` override means `wasm-pack` emits `moltendb_core.js` and `moltendb_core_bg.wasm` — matching the filenames expected by `@moltendb-web/core` with zero changes to the web repo.

The `rlib` target is included so crates.io accepts the package (crates.io requires at least one non-`cdylib` target). All WASM-specific code is gated behind `#![cfg(target_arch = "wasm32")]` so the `rlib` build on native targets is a no-op.

---

## Build

```bash
# Install wasm-pack (once)
curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh

# Build the WASM package
wasm-pack build moltendb-wasm --target web

# Output: moltendb-wasm/pkg/
#   moltendb_core.js
#   moltendb_core_bg.wasm
#   moltendb_core.d.ts
#   moltendb_core_bg.wasm.d.ts
```

---

## JavaScript API

### Initialisation (inside a Web Worker)

```ts
import init, { WorkerDb } from './wasm/moltendb_core.js';

// 1. Load the WASM binary
await init();

// 2. Open (or create) the OPFS database file
//    ✅ Use the static factory — NOT `new WorkerDb(name)` (deprecated)
const db = await WorkerDb.create("my_database");

// 3. Subscribe to real-time mutation events
db.subscribe((eventStr) => {
  const event = JSON.parse(eventStr);
  // event.event      → 'change' | 'delete' | 'drop'
  // event.collection → collection name
  // event.key        → document key
  // event.new_v      → version number (null on delete/drop)
  self.postMessage({ type: 'event', ...event });
});
```

> **Why `WorkerDb.create()` and not `new WorkerDb()`?**  
> Async constructors with `#[wasm_bindgen(constructor)]` produce invalid TypeScript bindings and are deprecated in `wasm-bindgen`. The named static factory `create()` is the correct pattern for async WASM initialisation.

### Handling messages

```ts
// Route a postMessage from the main thread to the correct handler
const result = db.handle_message({ action: 'get', collection: 'users', keys: 'u1' });

// Supported actions (identical to the HTTP server endpoints):
// 'get'      → query documents
// 'set'      → insert / upsert documents
// 'update'   → patch / merge documents
// 'delete'   → delete documents or drop a collection
// 'compact'  → compact the OPFS log file
// 'get_size' → return current OPFS file size in bytes
```

### Analytics

> ⚠️ **The analytics API is currently under development and not ready for production use.** The interface and behaviour may change without notice.

```ts
const resultStr = db.analytics(JSON.stringify({
  collection: 'events',
  metric: { type: 'COUNT' },
  where: { event_type: 'button_click' }
}));
// Returns a JSON string: { "result": 42, "metadata": { ... } }
```

---

## TypeScript shim

The generated `moltendb_core.d.ts` declares `WorkerDb` with the static factory:

```ts
export class WorkerDb {
  static create(dbName: string): Promise<WorkerDb>;
  handle_message(msg: { action: string; [key: string]: unknown }): unknown;
  subscribe(callback: (eventStr: string) => void): void;
  /** ⚠️ Under development — not ready for production use */
  analytics(queryJson: string): string;
}

export default function init(wasmUrl?: string | URL): Promise<void>;
```

---

## Auto-compaction thresholds

| Trigger | Default |
|---|---|
| Write count | Every 500 writes |
| File size | When OPFS file exceeds 5 MB |

Compaction rewrites the log to contain only the current state — removing superseded INSERT entries and DELETE tombstones. This shrinks the file and speeds up future startup replay. Compaction errors are logged to the browser console but never propagated.

---

## OPFS requirements

- Requires a **secure context** (HTTPS or `localhost`).
- Requires a **non-private browser window** (OPFS is unavailable in private/incognito mode in most browsers).
- Supported in Chrome 102+, Firefox 111+, Safari 15.2+.

---

## Part of the MoltenDB workspace

```
MoltenDB/
├── moltendb-core/     — pure engine (DashMap, WAL, query evaluator)
├── moltendb-wasm/     ← you are here
├── moltendb-auth/     — identity layer (JWT, Argon2, UserStore)
└── moltendb-server/   — network layer (Axum, TLS, CORS, CLI config)
```

See the [root README](../README.md) for the full architecture overview and the NPM packages built on top of this crate.
