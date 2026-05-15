# [1.0.0-rc3] (May 12, 2026)

### Breaking Changes
* **`_v` is now fully engine-managed on `/set`** — clients can no longer supply `_v` as an optimistic-lock guard on insert. The engine always sets `_v = 1` for new documents and increments it on every overwrite. Any document containing a `_`-prefixed field on `/set` or `/update` is now rejected with `400 Bad Request` without exception.
* **Renamed `createdAt` → `_createdAt` and `modifiedAt` → `_modifiedAt`** — all engine-managed timestamp fields now use the `_` prefix convention, consistent with `_key` and `_v`. Existing stored documents and any client code referencing `createdAt` or `modifiedAt` must be updated.

### Features
* **Bulk delete with `where` filters** — `/delete` now accepts a `"where"` clause using the same filter operators as `/get` (`$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$contains`, `$in`, `$nin`, `$or`, `$and`). The response includes a `"deleted"` count. Implemented via a new `Db::delete_filtered()` engine method (mirroring `Db::get_filtered()`) that uses a parallel rayon scan on native targets to collect matching keys before deleting them in a single transaction.
* **Reserved `_`-prefix field enforcement** — the handler layer now rejects any insert (`/set`) or update (`/update`) document that contains a field whose name starts with `_`, returning `400 Bad Request` with a descriptive error. This applies universally to every collection, with or without a JSON Schema registered, at `O(1)` cost per document.
* **`_createdAt` and `_modifiedAt` always returned** — both timestamp fields are now re-attached after field projection in `shape_doc`, so they appear in every response regardless of `fields` or `excludedFields` — consistent with how `_v` and `_key` are handled.

* **`count` defaults to 100, max 10000** — `/get` and `/delete` (bulk `where` mode) now default to returning/deleting at most 100 documents when `count` is not supplied. Supplying a value greater than 1000 returns a `400 Bad Request` error. This prevents accidental full-collection scans on large datasets.

### Features
* **`POST /stats` and `GET /stats` endpoints** -- returns document counts per collection. `POST /stats` with `{ "collection": "name" }` returns stats for a single collection; omitting `collection` (or using `GET /stats`) returns counts for all collections plus a `total`. TTL-aware: expired collections report `count: 0` and `expired: true` with their `expiresAt` timestamp. Both methods are available on the HTTP server and the WASM module (`action: "stats"`).

### Features (continued)
* **TTL eviction (collection-level)** — collections can now expire automatically via a TTL registered on `POST /schema` or inline on `POST /set` with a `"ttl"` field (seconds). The expiry clock resets to `now + ttl_secs` at the end of every insert batch — so the clock starts when the last write commits, not when the schema was registered. On expiry the **entire collection is dropped** in one O(1) call. `_expiresAt` is a **virtual field** — never stored inside documents, computed from the collection TTL map and injected into every response. The background sweep task uses an event-driven min-heap with one entry per collection (not per document), sleeping until the next collection expiry. Zero CPU when idle.
* **`/schema` accepts `ttl` without requiring `schema`** — `POST /schema` now accepts `"ttl"` independently of `"schema"`, allowing collection-level TTL defaults to be set on collections that have no JSON Schema validation.
* **`_seq` monotonic sequence field** — every document now receives a `_seq` field (u64) assigned by the engine at insert time. `_seq` is strictly increasing within a collection, never writable by clients, and always returned in every response alongside `_key`, `_v`, `_createdAt`, and `_modifiedAt`. Overwritten documents preserve their original `_seq` so insertion order is stable across updates.
* **`maxSize` capped collections** — collections can now be capped to a maximum document count via `POST /schema` with `"maxSize": N` or inline on `POST /set`. After each insert batch, if the collection exceeds `maxSize`, the oldest documents (lowest `_seq`) are evicted — keeping exactly `maxSize` documents at all times. `maxSize` is reported in `POST /stats` and `GET /stats` responses.

### Documentation
* Updated root `README.md` and `moltendb-core/README.md` with a **Reserved fields** table documenting `_key`, `_v`, `_createdAt`, `_modifiedAt`, and `_expiresAt`, the `_`-prefix enforcement rule, and the always-returned guarantee for all five fields. Added TTL documentation covering collection-level TTL via `/schema`, the virtual `_expiresAt` field, and the background sweep strategy.
* Expanded TTL documentation in both READMEs with a **sliding-window expiry design decision callout** — explicitly documenting that the TTL clock resets on every insert (not every access), that `/update` does not reset the clock, and that collection-level TTL is intentionally designed for ephemeral caches and analytics buffers rather than per-document expiry use cases (OTPs, reset tokens, session invalidation). Added a manual per-document expiry pattern using `POST /delete` with a `where` clause as the recommended alternative for security-sensitive expiry.
* Expanded `tests/requests_9_ttl.http` with new examples: §7 demonstrating that `/update` does not reset the TTL clock, §8 showing `where` queries on TTL collections, and §10a/§10b showing the manual per-document expiry pattern for use cases not suited to collection-level TTL.

---

# [1.0.0-rc2] (May 12, 2026)
### Breaking Changes
* **Encrypted WAL inner payload switched from JSON to MessagePack** — existing encrypted `.log` files written by `v1.0.0-rc1` or earlier are not readable by this version. Delete or migrate your encrypted WAL before upgrading.

### Performance
* **`Arc<str>` collection-key interning** — changed the outer `DashMap` key from `String` to `Arc<str>`. During bulk insert and WAL replay, all documents in the same collection share a single `Arc<str>` pointer instead of allocating a new `String` per document. Saves ~30 B per doc (~30 MB at 1M docs) and reduces allocator pressure during startup.
* **MessagePack in-memory storage** — switched the hot document map from `serde_json::Value` to `Box<[u8]>` (MessagePack bytes). Reduces steady-state RSS for 1M docs from ~4 GB to ~500 MB (~8× lower). Decoding to `Value` happens lazily on read; write paths encode via `rmp_serde`. Full analysis in `MEMORY_ANALYSIS.md`.
* **Parallel read paths (rayon)** — `get_filtered`, `get_all`, and `scan_top_n` now use `rayon` `par_iter` across DashMap shards on native targets. MsgPack decode (the dominant cost) runs across all CPU cores. Sequential fallback retained for `wasm32`.
* **Bounded per-worker heaps in `scan_top_n`** — rewrote sort-only paginated queries with rayon `fold` + `reduce` so each worker keeps its own small heap (size ≤ `cap`). Eliminates the 1M-element intermediate `Vec` that caused ~7s latency on sort-only queries; peak intermediate memory drops from O(N) to O(workers × cap).

### Removed (Backward Compatibility)
* **Dropped JSON-lines WAL fallback** — `log.rs` no longer accepts the legacy JSON-lines log format. The WAL is exclusively MessagePack length-prefixed. Databases written before `v1.0.0-rc1` must be migrated or discarded.
* **Dropped legacy snapshot format references** — `MOLTSNG2` (JSON body) and `MOLTSNAP` (uncompressed) snapshot formats were already rejected at load time; all remaining references and comments have been removed. Only `MOLTSNG3` (MsgPack + gzip) is supported.

### Bug Fixes
* **Eliminated memory spike during WAL replay and bulk insert** — changed `apply_entry` to take `LogEntry` by value so the `serde_json::Value` tree inside each entry is dropped immediately after MsgPack encoding. Previously both the `LogEntry` (~2.4 KB Value tree) and the new `Box<[u8]>` (~120 B) coexisted in RAM for every entry, causing ~2× peak memory during boot replay and stress inserts.
* Fixed `compact` method incorrectly placed inside `impl StorageBackend for OpfsStorage` (not a trait member) — moved to a separate `impl OpfsStorage` block in `wasm.rs`.
* Fixed unused-variable warnings (`state`, `hook`) in the default `compact_from_maps` impl under `#[cfg(not(feature = "schema"))]`.
* Fixed log file not being cleared after compaction on the encrypted storage path — `swap_log` now renames the `.tmp` file directly on the calling thread when there is no async writer (the case when `EncryptedStorage` delegates compaction to its inner storage).
* Implemented `compact_from_maps` on `EncryptedStorage` so the encrypted path now writes snapshots on compaction, reducing boot time from O(full WAL) to O(delta entries) — matching the non-encrypted path behaviour.

---
# [1.0.0-rc1] (May 9, 2026)

### Breaking Changes
* **Removed cold log / tiered storage** — `my_database.cold.log`, `TieredStorage` hot/cold promotion, `MmapLogReader`, and `HOT_TIER_MAX_BYTES` are gone. The `--storage-mode tiered` CLI flag is removed. All data now lives in a single log file + snapshot. `TieredStorage` is kept as a thin newtype over `AsyncDiskStorage` for compatibility but does no tiering.
* **Removed `--hot-threshold` / `MOLTENDB_HOT_THRESHOLD`** — the hot/cold eviction threshold no longer exists. All documents are loaded into RAM on startup. `evict_collection` and the `evict.rs` module are removed.
* **Removed auto-indexing** — `indexing.rs`, the `indexes` field on `Db`, `track_query`, and all INDEX log entries are removed. Queries always use full-collection scans. Indexes will be rebuilt from scratch in a future release.
* **Removed auto-compaction** — compaction no longer triggers automatically on write count or log size thresholds. Call `POST /snapshot` explicitly to compact. This eliminates surprise I/O spikes during bulk writes and gives full control over when the log is reset.

### Performance
* Rewrote `process_get.rs` — simplified from ~500 lines to ~230 lines; removed incorrect pre-sort fast path, extracted `shape_doc` helper, unified sort/truncate/skip pipeline.
* Added bounded heap fast path for `sort + count` queries with no joins or key filters — only `offset + count` documents ever live in RAM regardless of collection size, eliminating the memory-doubling spike previously observed on large collections (e.g. top-10 cheapest from 1M docs).
* Fixed snapshot `count` inflation (e.g. 979 000 instead of 100 000) caused by a race where `stream_log_into` replayed the old pre-truncation log on top of the already-loaded snapshot. Compaction now synchronously waits for the background file swap to complete before returning via a typed `WriterMsg::Compact` enum + condvar.

### Bug Fixes
* Fixed duplicate lines in the cold collection log — `promote_hot_to_cold` was called with the full database state (cold + hot combined) instead of only hot-tier entries. Moot after cold log removal but documented for history.

### Refactor
* Removed `write_compacted_log` from `disk/log.rs` — only used by the old cold-log promotion path.
* Removed re-exports of `write_compacted_log` and `write_snapshot` from `disk/mod.rs`.
* Deleted stale `my_database.cold.log` from project root.
* Removed stale auto-compaction references from README storage mode descriptions and the "Snapshots, Compaction & Data Safety" section.

---

# [1.0.0-rc0] (May 7, 2026)
> ⚠️ **WARNING:** This version was nuked and never made it to the public release.

### Reliability
* Implemented `AtomicBool` circuit breaker in `AsyncDiskStorage` to eliminate silent data loss on background disk I/O failure — when the background flush thread encounters a fatal `writeln!` or `flush` error it sets a shared `Arc<AtomicBool>` flag and stops accepting further writes; the core engine checks this flag at the top of every `insert`, `update`, and `delete` call and returns `DbError::StorageFault` immediately if it is set, preventing the in-memory state from diverging from what is persisted on disk
* Mapped `DbError::StorageFault` to `HTTP 503 Service Unavailable` in `process_set.rs` — clients now receive an explicit error response instead of a false `200 OK` when the storage layer is in a faulted state

### Performance
* Removed intermediate `Vec<LogEntry>` from snapshot loading path — entries are now streamed directly into the in-memory `DashMap` as they are read from disk, halving peak startup RAM usage for large snapshots (previously `~2×` snapshot file size, now `~1×`)
* Snapshot files are now gzip-compressed using `flate2` (pure Rust, WASM-compatible); typical JSON snapshots compress 3×–8×, significantly reducing disk usage and improving startup I/O on large datasets; magic header updated to `MOLTSNG2` for forward/backward compatibility — old `MOLTSNAP` snapshots are gracefully ignored and state is rebuilt from the WAL
* Added lazy `Db::get_filtered()` operation that filters documents *while* iterating the collection `DashMap`, only cloning matches; replaces the prior `get_all` → filter pattern in `process_get` for queries with WHERE but no joins. Reduces peak memory and time on sparse WHERE queries from O(total) to O(matches) — e.g. a query returning 84 matches from 1M docs no longer clones the other 999 916 documents
* Added bounded top-N streaming via `Db::scan_top_n()` — for `sort + count` (and optional `offset`) queries with no joins, documents are streamed directly into a max-heap of capacity `offset + count` with a peek-before-clone guard so docs that cannot beat the current worst candidate are never cloned. Peak memory is now O(offset + count) instead of O(matching_docs); eliminates the memory-doubling spike previously observed on §6 stress queries and brings response times under 1s on 1M-doc collections

### Reliability
* Replaced `.unwrap()` on disk I/O in the async storage background flush thread (`async_storage.rs`) with `match`/`if let Err` blocks that log errors via `tracing::error!` and return early — prevents silent panics on disk-full or lost file handle conditions
* Replaced `.lock().unwrap()` on the WASM handle mutex in `wasm.rs` (6 call sites) with `.lock().expect("db handle mutex poisoned")` — provides a clear, descriptive panic message if the mutex is ever poisoned by a prior panic

### Refactor
* Grouped the 8 parameters of `insert()` into `InsertParams<'a>` struct (`insert.rs`) to resolve Clippy's "too many arguments" warning; re-exported from `operations/mod.rs`; all call sites updated
* Grouped the 8 parameters of `update()` into `UpdateParams<'a>` struct (`update.rs`) for the same reason; re-exported from `operations/mod.rs`; all call sites updated

### Code Quality
* Fixed all ~44 Clippy warnings in `moltendb-core`: collapsed nested `if` statements, replaced redundant closures with direct constructor references (`DbError::Serialization`), removed unnecessary `Ok(…?)` in `encrypted.rs`, replaced `or_insert_with` with `or_default`, replaced manual `% 2 == 0` checks with `.is_multiple_of()`, replaced manual suffix stripping with `.strip_suffix()`
* Fixed all Clippy warnings in `moltendb-server`, `moltendb-auth`, and `moltendb-wasm`: `or_insert_with`, redundant cast, collapsible `if`, unnecessary `as_ref`/deref/borrow, and doc comment indentation issues
* Added `#![deny(warnings)]` to all four crates (`moltendb-core/src/lib.rs`, `moltendb-auth/src/lib.rs`, `moltendb-wasm/src/lib.rs`, `moltendb-server/src/main.rs`) — future warnings are now hard compile errors

### Documentation
* Updated all README files to reflect `1.0.0-rc` release candidate status: replaced `⚠️ Beta Software` notice with `🚀 Release Candidate (v1.0.0-rc)`, added `status-1.0.0-rc` badge to all six Rust crate READMEs, corrected test count badges (`88 passing` root, `59 passing` server), and updated `## Current limitations` heading in `moltendb-auth/README.md` from `v0.10.3` to `v1.0.0-rc`

# [0.10.3] (May 6, 2026)
### Bug Fixes
* Fixed stale document state after log replay: documents were not correctly removed from in-memory state during startup replay when a `DELETE` entry was encountered for a previously cold (disk-pointer) document

# [0.10.2] (May 4, 2026)
### Refactor
* Extracted `Db::open()` (native) and `Db::open_wasm()` (WASM) from `engine/mod.rs` into dedicated files `engine/open.rs` and `engine/open_wasm.rs`; `engine/mod.rs` now only declares and delegates
* Removed duplicate single-key `get` method; renamed `get_batch` to `get` — callers now pass `Vec<String>` and receive `HashMap<String, Value>`; all call sites and tests updated
* Removed duplicate single-key `delete` method; renamed `delete_batch` to `delete` — callers now pass `Vec<String>`; all call sites and tests updated
* Renamed `insert_batch` to `insert` across the entire codebase for consistency with the new `get`/`delete` naming
* Moved `compact`, `evict_collection`, and `recover_to` implementations from `engine/mod.rs` into dedicated files `operations/compact.rs`, `operations/evict.rs`, and `operations/recover.rs`; `engine/mod.rs` is now a thin delegation layer

# [0.10.1] (May 4, 2026)
Yanked due to a build issue.

# [0.10.0] (May 1, 2026)
### Features
* WebSocket JWT scope filtering — each connected client only receives change events for collections their token's scopes grant `read` access to; admin tokens (`*:*:*`) receive all events
* WebSocket revocation enforcement at connection time — revoked tokens are rejected immediately with a structured error (`{"error":"token_revoked", "detail":"..."}`) before the connection is accepted
* WebSocket revocation re-check on open connections — a background ticker checks every 30 seconds whether the authenticated token has been revoked since the connection was opened; if so, the client receives a `token_revoked` error and the connection is closed
* Distinct WebSocket auth error codes — each failure mode now returns a specific `error` code: `invalid_message`, `invalid_action`, `missing_token`, `invalid_token`, `token_revoked`
* Broadcast lag observability — `RecvError::Lagged` events are now logged as warnings instead of silently dropping the connection
* Configurable bind host — new `--host` CLI flag and `MOLTENDB_HOST` env var (default `0.0.0.0`); supports any IPv4/IPv6 address, enabling Docker and multi-interface deployments without recompilation
* In-memory mode — new `--in-memory` CLI flag and `MOLTENDB_IN_MEMORY` env var; bypasses the WAL and all disk I/O entirely, turning MoltenDB into a pure RAM cache (Redis-like); compaction and revocation-file persistence are automatically skipped; a startup warning is emitted to make the ephemeral nature explicit
* WASM in-memory mode — `WorkerDb.create()` now accepts an `in_memory` boolean as its ninth parameter; when `true`, OPFS is never opened and all data lives only in the browser's RAM — useful for ephemeral session caches or testing without touching persistent storage

# [0.9.0] (Apr 30, 2026)


### Features

* configurable max keys per request for core and wasm engines (`max_keys_per_request` in `DbConfig`, `--max-keys-per-request` CLI flag / `MOLTENDB_MAX_KEYS_PER_REQUEST` env var, `maxKeysPerRequest` param in `MoltenDb.create()`)
* dev mode: `--dev-mode` flag / `MOLTENDB_DEV_MODE` env var — runs the server over plain HTTP/WS instead of HTTPS/WSS, ignoring `--cert` and `--key` (for local development only)
* telemetry endpoints: `GET /system/health` (public liveness check) and `GET /system/metrics` (admin-only snapshot of uptime, process memory, host RAM/disk, and database internals — `hot_keys_count`, `hot_tier_threshold`, `wal_size_bytes`, `storage_mode`)



# [0.8.0](https://github.com/maximilian27/MoltenDB/compare/v0.7.0...v0.8.0) (Apr 30, 2026)


### Bug Fixes

* wasm error ([44478a9](https://github.com/maximilian27/MoltenDB/commit/44478a9f6060303b6091f46f1ceb36bfd1eeafe0))


### Features

* revoke token ([a96fb32](https://github.com/maximilian27/MoltenDB/commit/a96fb320aa735a9c0a1437fb56375e3cef18e59c))
* role based privileges ([8b535af](https://github.com/maximilian27/MoltenDB/commit/8b535afbfe5781803f2903977739fa6dcde7dd7d))



# [0.7.0](https://github.com/maximilian27/MoltenDB/compare/v0.6.3...v0.7.0) (Apr 28, 2026)


### Bug Fixes

* changelog merge conflict ([9f8a3fc](https://github.com/maximilian27/MoltenDB/commit/9f8a3fce25f1eb8e4fe3ab8cdc1effaaa8b3911e))


### Features

* post backup hook ([99f95e7](https://github.com/maximilian27/MoltenDB/commit/99f95e7b4f90025b7ff5f7a385a62b9bc467ec6e))



## [0.6.3](https://github.com/maximilian27/MoltenDB/compare/v0.6.2...v0.6.3) (Apr 28, 2026)


### Features

* post backup hook ([e3b0f55](https://github.com/maximilian27/MoltenDB/commit/e3b0f55dca219a9b4b0c5dc59f1dd2e0155949ac))



## [0.6.2](https://github.com/maximilian27/MoltenDB/compare/v0.4.0...v0.6.2) (Apr 27, 2026)


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



# [0.4.0](https://github.com/maximilian27/MoltenDB/compare/v0.3.0-beta.4...v0.4.0) (Apr 23, 2026)


### Bug Fixes

* wasm module build ([ddcae13](https://github.com/maximilian27/MoltenDB/commit/ddcae139cacd8be7fbef23ba0701ebbfc95c0cad))


### Features

* configurable hot threshold ([f1a38fb](https://github.com/maximilian27/MoltenDB/commit/f1a38fb432ebb49f3942fd9000fd5b8a0c15714e))
* expose hotThreshold, encryptionKey, writeMode, rateLimitRequests, rateLimitWindow and maxBodySize to web packages ([7450274](https://github.com/maximilian27/MoltenDB/commit/7450274769b6c9370ddfab9a999d1244be8647e4))
* oom protection ([db4cc96](https://github.com/maximilian27/MoltenDB/commit/db4cc96971c3378a6a656520b994d061adf33862))



