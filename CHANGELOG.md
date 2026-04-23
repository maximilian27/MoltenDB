# [0.4.0] (2026-04-23)

### Features

* **Web Package Configuration** — Updated `@moltendb-web/core` to expose `hotThreshold`, `encryptionKey`, `writeMode`, `rateLimitRequests`, `rateLimitWindow`, and `maxBodySize` in the `MoltenDb` constructor, bringing feature parity between the server and web environments.
* **WASM At-Rest Encryption** — Enabled transparent data encryption for browser-side storage using the `encryptionKey` option, powered by `XChaCha20-Poly1305`.
* **Isomorphic WASM Support** — The hybrid model works identically in the browser via OPFS, allowing large-scale local-first applications to run in memory-constrained browser tabs.


# [0.3.0-beta.5] (2026-04-23)


### Features

* **Hybrid Bitcask Storage** — Transitioned from a pure in-memory engine to a memory-efficient hybrid model. Documents are now stored as either `Hot` (RAM-resident JSON) or `Cold` (disk-resident byte offsets). This allows MoltenDB to handle datasets significantly larger than available RAM without crashing.
* **Configurable Hot/Cold threshold** — Added `--hot-threshold` CLI flag and `MOLTEN_HOT_THRESHOLD` environment variable to control the number of documents per collection kept in RAM. Default is 50,000.
* **Auto-eviction on writes** — The engine now automatically checks collection size during `insert_batch` and `update` operations, moving documents to the `Cold` (disk) tier if the threshold is exceeded.
* **Performance/Durability Tradeoff** — Documented the `writeMode` tradeoff: `async` (default) is blazing fast for high-throughput, while `sync` provides maximum durability by flushing to physical storage before returning.


### Architecture

* **RecordPointer & DocumentState** — Internal refactoring to support lazy deserialization and targeted byte-offset reads via the `StorageBackend` trait.


# [0.2.0-beta.4] (2026-04-17)


### Architecture

* **`moltendb-wasm` extracted as a dedicated crate** — WASM bindings (`WorkerDb`, `wasm-bindgen` glue) have been moved out of `moltendb-core` into a new `moltendb-wasm` crate. `moltendb-core` is now a pure `rlib` with zero WASM dependencies; `moltendb-wasm` is the thin `cdylib` browser adapter. The workspace now has four members: `moltendb-core`, `moltendb-wasm`, `moltendb-auth`, `moltendb-server`.
* **Handler and validation deduplication** — `handlers/` and `validation.rs` were duplicated across `moltendb-core` and `moltendb-server`. The server-side copies have been deleted; `moltendb-server` now consumes `moltendb_core::handlers` and `moltendb_core::validation` directly. Single source of truth.
* **Workspace-level release profile** — `[profile.release]` (`opt-level = "z"`, `lto = true`, `codegen-units = 1`, `strip = true`, `panic = "abort"`) moved to the root `Cargo.toml` so it applies uniformly to all crates including the WASM build.


### Bug Fixes

* missing import ([6c47a3b](https://github.com/maximilian27/MoltenDB/commit/6c47a3b702f73b297c850c7cda1b2952744c0d8e))
* missing import ([2b3e531](https://github.com/maximilian27/MoltenDB/commit/2b3e53171ad173ca6448affd800c796b480fa7b1))



# [0.2.0-beta.4](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-beta.2...v0.2.0-beta.4) (2026-04-16)


### Bug Fixes

* wasm module ([fc4101c](https://github.com/maximilian27/MoltenDB/commit/fc4101c15a99079cc0fff0abc045e45c93fd8b41))



# [0.1.0-beta.2](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-alpha.25...v0.1.0-beta.2) (2026-04-08)


### Bug Fixes

* async writer data loss on graceful shutdown ([5dd2514](https://github.com/maximilian27/MoltenDB/commit/5dd25141336924d3269965350c841b9b166016a2))



# [0.1.0-alpha.25](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-alpha.23...v0.1.0-alpha.25) (2026-04-07)


### Performance Improvements

* dev only deps for reqwest ([9910666](https://github.com/maximilian27/MoltenDB/commit/9910666cca2de2e31d67515813d01b1299ca47fb))



# [0.1.0-alpha.23](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-alpha.21...v0.1.0-alpha.23) (2026-04-06)



