# [0.10.1] (2026-05-04)
### Refactor
* Extracted `Db::open()` (native) and `Db::open_wasm()` (WASM) from `engine/mod.rs` into dedicated files `engine/open.rs` and `engine/open_wasm.rs`; `engine/mod.rs` now only declares and delegates
* Removed duplicate single-key `get` method; renamed `get_batch` to `get` — callers now pass `Vec<String>` and receive `HashMap<String, Value>`; all call sites and tests updated
* Removed duplicate single-key `delete` method; renamed `delete_batch` to `delete` — callers now pass `Vec<String>`; all call sites and tests updated
* Renamed `insert_batch` to `insert` across the entire codebase for consistency with the new `get`/`delete` naming
* Moved `compact`, `evict_collection`, and `recover_to` implementations from `engine/mod.rs` into dedicated files `operations/compact.rs`, `operations/evict.rs`, and `operations/recover.rs`; `engine/mod.rs` is now a thin delegation layer

# [0.10.0] (2026-05-01)
### Features
* WebSocket JWT scope filtering — each connected client only receives change events for collections their token's scopes grant `read` access to; admin tokens (`*:*:*`) receive all events
* WebSocket revocation enforcement at connection time — revoked tokens are rejected immediately with a structured error (`{"error":"token_revoked", "detail":"..."}`) before the connection is accepted
* WebSocket revocation re-check on open connections — a background ticker checks every 30 seconds whether the authenticated token has been revoked since the connection was opened; if so, the client receives a `token_revoked` error and the connection is closed
* Distinct WebSocket auth error codes — each failure mode now returns a specific `error` code: `invalid_message`, `invalid_action`, `missing_token`, `invalid_token`, `token_revoked`
* Broadcast lag observability — `RecvError::Lagged` events are now logged as warnings instead of silently dropping the connection
* Configurable bind host — new `--host` CLI flag and `MOLTENDB_HOST` env var (default `0.0.0.0`); supports any IPv4/IPv6 address, enabling Docker and multi-interface deployments without recompilation
* In-memory mode — new `--in-memory` CLI flag and `MOLTENDB_IN_MEMORY` env var; bypasses the WAL and all disk I/O entirely, turning MoltenDB into a pure RAM cache (Redis-like); compaction and revocation-file persistence are automatically skipped; a startup warning is emitted to make the ephemeral nature explicit
* WASM in-memory mode — `WorkerDb.create()` now accepts an `in_memory` boolean as its ninth parameter; when `true`, OPFS is never opened and all data lives only in the browser's RAM — useful for ephemeral session caches or testing without touching persistent storage

# [0.9.0] (2026-04-30)


### Features

* configurable max keys per request for core and wasm engines (`max_keys_per_request` in `DbConfig`, `--max-keys-per-request` CLI flag / `MOLTENDB_MAX_KEYS_PER_REQUEST` env var, `maxKeysPerRequest` param in `MoltenDb.create()`)
* dev mode: `--dev-mode` flag / `MOLTENDB_DEV_MODE` env var — runs the server over plain HTTP/WS instead of HTTPS/WSS, ignoring `--cert` and `--key` (for local development only)
* telemetry endpoints: `GET /system/health` (public liveness check) and `GET /system/metrics` (admin-only snapshot of uptime, process memory, host RAM/disk, and database internals — `hot_keys_count`, `hot_tier_threshold`, `wal_size_bytes`, `storage_mode`)



# [0.8.0](https://github.com/maximilian27/MoltenDB/compare/v0.7.0...v0.8.0) (2026-04-30)


### Bug Fixes

* wasm error ([44478a9](https://github.com/maximilian27/MoltenDB/commit/44478a9f6060303b6091f46f1ceb36bfd1eeafe0))


### Features

* revoke token ([a96fb32](https://github.com/maximilian27/MoltenDB/commit/a96fb320aa735a9c0a1437fb56375e3cef18e59c))
* role based privileges ([8b535af](https://github.com/maximilian27/MoltenDB/commit/8b535afbfe5781803f2903977739fa6dcde7dd7d))



# [0.7.0](https://github.com/maximilian27/MoltenDB/compare/v0.6.3...v0.7.0) (2026-04-28)


### Bug Fixes

* changelog merge conflict ([9f8a3fc](https://github.com/maximilian27/MoltenDB/commit/9f8a3fce25f1eb8e4fe3ab8cdc1effaaa8b3911e))


### Features

* post backup hook ([99f95e7](https://github.com/maximilian27/MoltenDB/commit/99f95e7b4f90025b7ff5f7a385a62b9bc467ec6e))



## [0.6.3](https://github.com/maximilian27/MoltenDB/compare/v0.6.2...v0.6.3) (2026-04-28)


### Features

* post backup hook ([e3b0f55](https://github.com/maximilian27/MoltenDB/commit/e3b0f55dca219a9b4b0c5dc59f1dd2e0155949ac))



## [0.6.2](https://github.com/maximilian27/MoltenDB/compare/v0.4.0...v0.6.2) (2026-04-27)


### Bug Fixes

* add transactions to update ([0c4681d](https://github.com/maximilian27/MoltenDB/commit/0c4681d8b4bec2e89bf81a0cefe7fb5c30db0bb5))
* fix disk log issues ([7b9c7a0](https://github.com/maximilian27/MoltenDB/commit/7b9c7a03ddb6a488de8265177550e67bd272cfae))
* load snapshot in memory ([a7b6242](https://github.com/maximilian27/MoltenDB/commit/a7b62427564f6992145b18f982ec4b3cf100a468))
* persist delete functionality across restart ([d21f56c](https://github.com/maximilian27/MoltenDB/commit/d21f56ca621ecbfd631c1912f646cb0e64904a4b))
* wasm compilation issues ([cf7f2fd](https://github.com/maximilian27/MoltenDB/commit/cf7f2fd4b3acb63574f9deb956b8c0677be233c9))


### Features

* add timestamp metadata ([65b7363](https://github.com/maximilian27/MoltenDB/commit/65b73634189297ad0a55028608e7642d34d57005))
* expose snapshot endpoint ([f8a9b09](https://github.com/maximilian27/MoltenDB/commit/f8a9b091331dcddc91ab6c8e27b381dc5d08c1cd))
* point in time recovery ([619239d](https://github.com/maximilian27/MoltenDB/commit/619239db6597be4ee945fcd7caa8812ebcd4007a))
* schema validation ([85757a2](https://github.com/maximilian27/MoltenDB/commit/85757a2f0f959dbe6bf9513ea5fb657c2acd97bf))
* versioned snapshot and backup management ([0a46217](https://github.com/maximilian27/MoltenDB/commit/0a46217ac6dc06686b1f2760fc3ab290be969ffc))
* WAL transaction markers ([51f7ecd](https://github.com/maximilian27/MoltenDB/commit/51f7ecd04c42bbbda5412601807856154f9ba32e))



# [0.4.0](https://github.com/maximilian27/MoltenDB/compare/v0.3.0-beta.4...v0.4.0) (2026-04-23)


### Bug Fixes

* wasm module build ([ddcae13](https://github.com/maximilian27/MoltenDB/commit/ddcae139cacd8be7fbef23ba0701ebbfc95c0cad))


### Features

* configurable hot threshold ([f1a38fb](https://github.com/maximilian27/MoltenDB/commit/f1a38fb432ebb49f3942fd9000fd5b8a0c15714e))
* expose hotThreshold, encryptionKey, writeMode, rateLimitRequests, rateLimitWindow and maxBodySize to web packages ([7450274](https://github.com/maximilian27/MoltenDB/commit/7450274769b6c9370ddfab9a999d1244be8647e4))
* oom protection ([db4cc96](https://github.com/maximilian27/MoltenDB/commit/db4cc96971c3378a6a656520b994d061adf33862))



