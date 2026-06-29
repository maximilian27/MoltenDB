# [1.0.0-rc11] (Jun 11, 2026)

### Refactoring

* **`moltendb-server` internal structure** — extracted shared types and constants into dedicated modules, mirroring
  the pattern used in `moltendb-core` and `moltendb-auth`:
  - `src/types.rs` — `AppState` type alias for the 6-tuple Axum state (`Db`, `UserStore`, `usize`, `usize`, `String`,
    `u64`). All handler signatures and `ws_handler` now use `State<AppState>` instead of repeating the full tuple type.
  - `src/constants.rs` — named constants replacing all magic numbers and strings: `DEFAULT_DELEGATE_TTL_SECS` (3600),
    `DEFAULT_REVOKE_TTL_SECS` (86400), `DEFAULT_ROOT_TOKEN_TTL_SECS` (86400), `ACTION_READ`, `ACTION_WRITE`,
    `ACTION_DELETE`, `ADMIN_SCOPE`, `REVOCATION_PRUNE_INTERVAL_SECS` (60), `RATE_LIMIT_CLEANUP_INTERVAL_SECS` (300),
    `GRACEFUL_SHUTDOWN_TIMEOUT_SECS` (30).

### New Features

* **Configurable root token TTL** — the TTL of the root JWT issued by `POST /auth/login` is now configurable via
  `--root-token-ttl <secs>` (CLI flag) or `MOLTENDB_ROOT_TOKEN_TTL` (environment variable). Defaults to `86400`
  seconds (24 hours). `create_token(username, ttl_secs)` now accepts the TTL as a parameter instead of hardcoding
  86400. Scoped tokens minted via `POST /auth/delegate` are unaffected — they continue to use the `ttl_secs` field
         in the request body.

* **`POST /auth/refresh` — client-initiated scoped token refresh** — clients can exchange a valid, non-expired scoped
  token for a new one with the same `sub` and `scopes` but a fresh `exp` and a new `jti`. The old `jti` is immediately
  added to the `RevocationStore` after the new token is issued, preventing replay attacks. Admin tokens (`*:*:*`) are
  explicitly not refreshable — the endpoint returns `403 Forbidden` with a clear message directing the caller to
  re-delegate via `POST /auth/delegate`. Expired or revoked tokens return `401 Unauthorized`. An optional
  `{ "ttl_secs": <u64> }` request body controls the new token's TTL (defaults to 3600 seconds). Response:
  `{ "token": "<new_jwt>", "jti": "<new_jti>", "expires_in": <ttl_secs> }`. New public API in `moltendb-auth`:
  `refresh_scoped_token(old_token, new_ttl_secs, revocation_store) -> Result<(String, String), AuthError>`.
  New `AuthError` enum (`InvalidToken`, `TokenExpired`, `TokenRevoked`, `RefreshNotAllowed`, `RoleNotFound`,
  `HashError`)
  is now the unified error type for all public `moltendb-auth` API functions — `create_token`, `create_scoped_token`,
  `verify_token`, `hash_password`, `verify_password`, `refresh_scoped_token`, and `UserStore::new` all return
  `AuthError`
  instead of raw `jsonwebtoken::errors::Error` or `bcrypt::BcryptError`.

### Security

* **HMAC-SHA256 integrity protection for the revocation store** — `RevocationStore::save_to_file` now signs the
  `entries` JSON with HMAC-SHA256 (keyed with `JWT_SECRET`) and writes `{ "entries": {...}, "sig": "<hex>" }` to disk.
  `load_from_file` verifies the signature before trusting any entries; a missing or invalid `sig` field causes the
  server to abort startup with a `CRITICAL` log (fail-closed). A missing file is still treated as a clean first
  startup. This closes the attack vector where an attacker with filesystem access could delete or truncate the
  revocation file to silently re-validate previously revoked tokens. `hmac`, `sha2`, and `hex` are now explicit
  dependencies of `moltendb-auth`. `load_from_file` return type changed from `Self` to `Result<Self, String>`.

* **Async-safe revocation persistence** — `RevocationStore::save_to_file` is now `async` and uses `tokio::fs::write`
  instead of `std::fs::write`. Previously, calling it inside the async `handle_revoke` handler blocked the Tokio worker
  thread for the duration of the JSON serialization and disk flush, starving other concurrent requests. The background
  prune task in `main.rs` and the `handle_revoke` handler both `.await` the call. `tokio` (with the `fs` feature) is
  now an explicit dependency of `moltendb-auth`.

* **Fail-closed on missing `jti` in `auth_middleware`** — tokens with an empty `jti` field (legacy tokens or
  crafted tokens exploiting `#[serde(default)]`) previously bypassed the revocation check entirely. The middleware now
  rejects any token without a `jti` with `401 Unauthorized`. The `#[serde(default)]` attribute has been removed from
  the `jti` field in `Claims` — all tokens minted by this crate have always included a `jti`. Option A (strict /
  fail-closed) was chosen.

* **Eliminate `get_secret()` lock contention** — `JWT_SECRET` is now read from the environment exactly once at process
  startup via `std::sync::OnceLock` and cached for the lifetime of the process. Previously, `get_secret()` called
  `std::env::var("JWT_SECRET")` on every call to `verify_token` and `create_scoped_token`, acquiring a global OS-level
  lock on each authenticated request. Under concurrent load this caused thread contention. No API surface change.

* **Fix silent admin lockout in `UserStore::new`** — if `bcrypt` failed to hash the admin password at startup (e.g. RNG
  exhaustion, OS error), the server would boot with an empty user store and permanently lock out the admin with no log,
  no panic, and no indication. `UserStore::new` now returns `Result<Self, AuthError>`; the server aborts
  startup with a `CRITICAL` log line if hashing fails. This is a minor breaking change to the `UserStore::new`
  signature — callers must handle the `Result`.

---

# [1.0.0-rc10] (Jun 5, 2026)

### ⚠️ Breaking Changes — Older log/WAL files are NOT compatible with this release

* **System field tokenization in storage and WAL** — system field keys (`_v`, `_key`, `_seq`, `_createdAt`,
  `_modifiedAt`, `_expiresAt`) are no longer stored as strings on disk or in the Write-Ahead Log. They are now encoded
  as single-byte negative MsgPack FixInt tokens (`-1` through `-6`), reducing per-document storage overhead by ~30 bytes
  and enabling O(1) key matching during scans. Databases written by older versions of MoltenDB will fail to load — the
  storage format version has been bumped (`MOLTSNG4`) and any file with the old magic bytes will be rejected with an
  explicit `Unsupported database file version` error.
* **Log command tokenization in WAL** — WAL command strings (`INSERT`, `DELETE`, `DROP`, `INDEX`, `SCHEMA`, `ENC`,
  `TX_BEGIN`, `TX_COMMIT`) are now stored as single-byte negative MsgPack FixInt tokens (`-10` through `-17`) instead of
  plain strings. Tokens `-1` through `-9` are reserved for future system fields. Log files written by older versions
  cannot be replayed by this release.
* **In-memory representation changed** — system field keys and log command identifiers are now kept as compact integer
  string keys (e.g. `"-1"`, `"-10"`) in RAM throughout the engine. Expansion to human-readable names (`_createdAt`,
  `INSERT`, etc.) only occurs at the public API boundary when returning results to clients. This reduces heap
  allocations per document and improves cache locality.

### Performance

* **~30 bytes saved per document on disk and in WAL** — all six system field keys are now 1 byte each instead of 3–12
  bytes.
* **O(1) system field key matching during raw MsgPack scans** — key comparison is now a single `if byte < 0` branch
  instead of a string comparison loop.
* **Reduced heap allocations per document in RAM** — system field keys and log command strings are stored as short
  integer strings rather than full human-readable `String` allocations.
* **Raw MsgPack WHERE evaluation (`evaluate_where_msgpack`)** — the full WHERE clause (including `$or`/`$and`
  recursion) is now evaluated directly on raw MsgPack bytes without deserialising the document first.
  `scan_top_n` and the `get_filtered` fallback path both use this new evaluator, so non-matching documents are
  never decoded to `serde_json::Value`. For selective queries on large collections (e.g. 1 in 1000 docs matching)
  this reduces deserialisations from O(N) to O(matching docs).
* **Single-pass multi-field extractor (`find_msgpack_values_multi`)** — extracts N requested field values from a
  document in one left-to-right byte sweep, grouping nested paths by their common prefix so each sub-map is
  entered only once. Used by the analytics engine to achieve ~3× speedup over N sequential single-field scans.
* **`scan_top_n` predicate now operates on raw bytes** — the predicate closure signature changed from
  `Fn(&Value) -> bool` to `Fn(&str, &[u8]) -> bool`; deserialization only occurs after the predicate passes,
  eliminating unnecessary allocations for documents that are filtered out.

### Bug Fixes

* **`keys` field rejected by GET handler** — `PayloadField::Keys` was missing from `GET_ALLOWED`, causing any
  request that included a `keys` array to be rejected with a 400 `Unknown property` error.
* **`_v` (version) not returned in GET responses** — `shape_doc` was reading system field keys using internal
  `IKEY_*` token strings, but documents arriving at that point had already been expanded to public names by
  `expand_system_fields`. The version field was silently dropped. Fixed by using the public name constants
  (`SystemFields::VERSION` etc.) throughout `shape_doc`.

---

# [1.0.0-rc9] (Jun 5, 2026)

### Refactor

* **Refactor core crate. Improved naming convention and structure**

### Performance

* **Store _createdAt and _modifiedAt as unix timestamps and convert to ISO only when the response gets processed**
* **Shorthand properties for _createdAt, _modifiedAt, _seq and _expiresAt reducing the memory footprint by ~5%**

### Breaking Changes (Security)

* **Remove post snapshot hook script for security reasons**

# [1.0.0-rc8] (May 28, 2026)

### Bug Fixes

* **Remove duplicate profiles from wasm build workflow**

# [1.0.0-rc7] (May 28, 2026)

### CI

* **Custom build profile for wasm crate when syncing with web repo**

# [1.0.0-rc6] (May 27, 2026)

### Docs

* **Added benchmarking docs**
* **Added transparency docs**

# [1.0.0-rc5] (May 21, 2026)

### Performance

* **`$ct`/`$contains` added to the MsgPack fast path** -- substring-match predicates (e.g.
  `{ "model": { "$ct": "Pro" } }`) now evaluate directly on raw MsgPack bytes via `evaluate_predicate_msgpack` instead
  of falling back to full `rmp_serde` deserialization per document. The check is case-insensitive. Crucially, `$ct` now
  also works on **array fields** (e.g. `{ "tags": { "$ct": "gaming" } }` where `tags` is a string array) — the evaluator
  iterates the MsgPack array elements and returns `true` if any element contains the needle. `is_fast_path_op` in
  `fetch.rs` includes `$ct`/`$contains`, so mixed WHERE clauses also avoid full deserialization.
* **Parallel Phase 3 decode** -- `get_filtered`, `get_all` now use `par_iter()` in Phase 3 (decoding the page-sized
  subset of documents) instead of a sequential `iter()`. For `count: 100` with heavy documents this is free
  parallelism — the decode work is embarrassingly parallel and scales near-linearly with available cores. The wasm32
  paths remain sequential (no rayon on wasm32).
* **Multi-condition WHERE clauses now use the MsgPack fast path** — previously, any WHERE clause with more than one
  condition (e.g. `{ "price": { "$gte": 1500, "$lte": 2500 } }` or `{ "in_stock": true, "price": { "$lt": 1000 } }`)
  fell through to full `rmp_serde` deserialization for every document because `extract_single_field_predicate` required
  exactly one field with exactly one operator. The new `extract_fast_predicates` helper flattens the entire WHERE clause
  into a flat list of `(field, op, value)` triples and evaluates them all directly on raw MsgPack bytes via
  `evaluate_predicate_msgpack`. All conditions are AND-ed together without ever calling `rmp_serde::from_slice`. This
  covers:
  - **Multi-operator on the same field**: `{ "price": { "$gte": 1500, "$lte": 2500 } }` → two raw-byte comparisons per
    doc
  - **Multi-field predicates**: `{ "in_stock": true, "price": { "$lt": 1000 } }` → two raw-byte comparisons per doc
  - **Any combination of `$eq`, `$ne`, `$in`, `$nin`, `$gt`, `$gte`, `$lt`, `$lte`** across any number of fields
* **Full deserialization fallback preserved** — clauses containing logical operators (`$or`, `$and`) or unknown/custom
  operators still fall back to `evaluate_where` with full deserialization, exactly as before.
* **Scalar fast-path extended to `$gt`/`$gte`/`$lt`/`$lte`** -- `evaluate_predicate_msgpack` now handles all four
  numeric range operators in addition to the existing `$eq`/`$ne`/`$in`/`$nin`, so even non-SIMD paths (wasm32,
  prefix-filtered queries) avoid full `rmp_serde` deserialization for numeric comparisons.
* **Three-phase scan eliminates deserialization memory spikes** -- `get_filtered` (native path) now runs in three
  distinct phases: (1) parallel predicate scan collecting only matching `String` keys â€” no `Value` allocation; (2)
  sort + page-slice on the key list; (3) MsgPack decode for the page-sized subset only. For `$ne`/`$nin` queries
  matching 80%+ of a large collection, this avoids materialising hundreds of thousands of decoded `serde_json::Value`
  objects simultaneously, eliminating the x8â€“x10 RSS spike previously observed on those operators.
* **Full MsgPack fast-path for `$ne`, `$in`, `$nin`, and nested-field predicates** -- extended the raw-bytes predicate
  evaluator (`evaluate_predicate_msgpack`) to cover all four common operators with full dot-notation support (e.g.
  `specs.cpu.brand`). Previously only `$eq` on plain top-level strings had a fast path; all other operators fell through
  to `rmp_serde::from_slice` per document, causing rayon workers to materialise the entire collection in memory
  simultaneously. The new path touches ~16â€“32 bytes per document for the common case.
* **`get_all` also uses three-phase paging** -- the unfiltered path mirrors the same (seq, key) collect â†’ sort â†’
  page â†’ decode pattern, so sorted full-collection scans no longer decode documents that are outside the requested
  page window.

### Bug Fixes

* **Duplicate documents across paginated sorted queries** -- `HeapItem::Ord` previously broke ties by sort-value only,
  causing non-deterministic heap eviction when multiple documents share the same sort value. Ties are now broken by
  `key`, making the bounded heap fully deterministic. Both the rayon `push_into` guard and the wasm32 sequential path
  use the same `HeapItem::cmp` (value + key) comparator, so eviction decisions are consistent with heap ordering.

### Improvements

* **wasm32 scan paths now identical to native** -- `get_filtered` and `get_all` on wasm32 now use the same three-phase (
  collect `(seq, key)` -> sort by `_seq` -> page -> decode) pattern as the native rayon path. Previously the wasm32
  branches used a streaming early-stop loop that decoded documents before paging and applied `offset`/`count` inline,
  producing non-deterministic ordering and incorrect pagination. Both targets now sort by insertion order (`_seq`) and
  decode only the page-sized subset.
* **Default sort order changed from `_key` (alphabetical) to `_seq` (insertion order)** -- when no `sort` field is
  provided, `get_filtered` and `get_all` now sort results by the document's `_seq` counter before applying `offset`/
  `count`. This matches the natural-order expectation of every major document database (MongoDB, CouchDB, Firestore) and
  avoids surprising alphabetical ordering when keys are user-supplied strings. A new `read_msgpack_seq` helper extracts
  `_seq` directly from raw MsgPack bytes without full deserialization; docs without `_seq` sort last (`u64::MAX`
  fallback).

---

# [1.0.0-rc4] (May 19, 2026)

### Performance

* **Zero-copy binary predicate evaluation** -- swapped `serde_json` deserialization in `get_filtered` with a raw `rmp`
  byte-scanner. This avoids deserializing the entire document into an allocated `serde_json::Value` during full-table
  scans for simple queries, vastly reducing CPU overhead and memory allocation churn, yielding ~3x faster queries on
  large collections.

### Bug Fixes

* **Tenant scoping in GET queries** -- pushed `_allowed_prefixes` evaluation directly into `fetch_documents` and the
  core `get_filtered` / `get_all` closures. Prefix gating is now evaluated before `limit` and `offset` truncation to
  prevent silent query results dropping.
* **Pagination stability** -- changed `get_filtered` and `get_all` to return `Vec<(String, Value)>` instead of
  `HashMap`. Ordered sorting based on `key` is now maintained before truncation, preventing non-deterministic pagination
  scrambling.
* **OOM on simple queries** -- modified `get_all` to accept `offset` and `limit` parameters, enabling early loop exit
  and preventing massive full-collection heap allocations on simple queries.
* **Early prefix gating** -- improved efficiency by evaluating simple conditions (e.g. `_allowed_prefixes`) on raw
  document keys before performing deep JSON parsing and full `WHERE` predicate evaluation.
* **Parameter rename** -- renamed the parameter `limit` to `count` in the core engine's GET functionality signatures (
  `get_filtered` and `get_all`) to match the existing naming convention across the API.
* **WASM: OOM on OPFS compaction** -- fixed `OpfsStorage` failing to implement `compact_from_maps`. The compaction
  process previously aggregated all documents into a massive byte vector before writing to OPFS. It now serializes
  documents sequentially directly from the DashMap and writes in 64KB chunks, preventing OOM crashes in the Web Worker
  when compacting large datasets.
* **WASM: OPFS storage format inconsistency** -- OPFS backend (`wasm.rs`) still used plain-text JSON with newline
  separators for logging, causing high disk usage and string encoding/decoding overhead in the browser. Aligned with
  native engines by switching to binary length-prefixed MessagePack (`rmp_serde`). The reader now streams the file in
  chunks instead of loading the entire log into a `String`, preventing Out of Memory (OOM) crashes in the browser worker
  on large datasets.
* **WASM: `time not implemented on this platform` panic** -- replaced `std::time::SystemTime` with
  `web_time::SystemTime` in `engine/operations/ttl.rs` and `engine/storage/disk/snapshot.rs`. Both files were using the
  standard library time API which panics unconditionally on `wasm32` targets. `web_time` was already a dependency of
  `moltendb-core`.
* **TTL expiry: re-inserted documents incorrectly received `_v: 2` instead of `_v: 1`** -- TTL expiry used lazy
  eviction (documents hidden on read but never removed from the `DashMap`). When the same keys were re-inserted after
  expiry, `insert.rs` found the stale documents in memory and incremented their `_v` counter. Fixed in `db.insert()` (
  `engine/mod.rs`): before calling `operations::insert`, the engine now checks whether the target collection has expired
  and, if so, physically evicts it via `operations::delete_collection()`.
* **TTL eviction is now durable across restarts** -- the previous fix cleared expired documents from memory but left the
  old `INSERT` entries in the WAL. A process restart before compaction would replay those entries and restore the stale
  documents, causing the `_v` counter bug to reappear. The eviction now calls `operations::delete_collection()` which
  writes a `TX_BEGIN â†’ DROP â†’ TX_COMMIT` sequence to the WAL before removing the collection from memory, so the
  eviction survives WAL replay.
* **TTL expiry now survives restarts** -- `ttl_expiry` was a purely in-memory map that was never persisted. After a
  reload, the map was empty so `process_get` no longer hid expired documents â€” they became fully visible again. Fixed
  by writing a `TTL_EXPIRY` WAL entry each time an expiry timestamp is set, and restoring it during WAL replay in
  `apply_entry`. If the expiry has already passed at replay time, the collection is evicted from state immediately so
  stale documents never surface after a reload.

---

# [1.0.0-rc3] (May 12, 2026)

### Breaking Changes

* **`_v` is now fully engine-managed on `/set`** -- clients can no longer supply `_v` as an optimistic-lock guard on
  insert. The engine always sets `_v = 1` for new documents and increments it on every overwrite. Any document
  containing a `_`-prefixed field on `/set` or `/update` is now rejected with `400 Bad Request` without exception.

### Features

* **Bulk delete with `where` filters** -- `/delete` now accepts a `"where"` clause using the same filter operators as
  `/get` (`$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$contains`, `$in`, `$nin`, `$or`, `$and`). The response includes
  a `"deleted"` count. Implemented via a new `Db::delete_filtered()` engine method (mirroring `Db::get_filtered()`) that
  uses a parallel rayon scan on native targets to collect matching keys before deleting them in a single transaction.
* **Reserved `_`-prefix field enforcement** -- the handler layer now rejects any insert (`/set`) or update (`/update`)
  document that contains a field whose name starts with `_`, returning `400 Bad Request` with a descriptive error. This
  applies universally to every collection, with or without a JSON Schema registered, at `O(1)` cost per document.

* **`count` defaults to 100, max 1000** -- `/get` and `/delete` (bulk `where` mode) now default to returning/deleting at
  most 100 documents when `count` is not supplied. Supplying a value greater than 1000 returns a `400 Bad Request`
  error. This prevents accidental full-collection scans on large datasets.

### Features

* **`POST /stats` and `GET /stats` endpoints** -- returns document counts per collection. `POST /stats` with
  `{ "collection": "name" }` returns stats for a single collection; omitting `collection` (or using `GET /stats`)
  returns counts for all collections plus a `total`. TTL-aware: expired collections report `count: 0` and
  `expired: true` with their `expiresAt` timestamp. Both methods are available on the HTTP server and the WASM module (
  `action: "stats"`).

### Features (continued)

* **TTL eviction (collection-level)** -- collections can now expire automatically via a TTL registered on `POST /schema`
  or inline on `POST /set` with a `"ttl"` field (seconds). The expiry clock resets to `now + ttl_secs` at the end of
  every insert batch -- so the clock starts when the last write commits, not when the schema was registered. On expiry
  the **entire collection is dropped** in one O(1) call. `_expiresAt` is a **virtual field** -- never stored inside
  documents, computed from the collection TTL map and injected into every response. The background sweep task uses an
  event-driven min-heap with one entry per collection (not per document), sleeping until the next collection expiry.
  Zero CPU when idle.
* **`/schema` accepts `ttl` without requiring `schema`** -- `POST /schema` now accepts `"ttl"` independently of
  `"schema"`, allowing collection-level TTL defaults to be set on collections that have no JSON Schema validation.
* **`maxSize` capped collections** -- collections can now be capped to a maximum document count via `POST /schema` with
  `"maxSize": N` or inline on `POST /set`. After each insert batch, if the collection exceeds `maxSize`, the oldest
  documents (lowest `_seq`) are evicted -- keeping exactly `maxSize` documents at all times. `maxSize` is reported in
  `POST /stats` and `GET /stats` responses.
* **`_seq`, `_createdAt`, `_modifiedAt`, `_expiresAt` are now opt-in** -- these fields are no longer returned by
  default. They must be explicitly requested via a `fields` projection (e.g. `"fields": ["brand", "_createdAt"]`).
  `_key` and `_v` remain always-present protocol primitives that cannot be suppressed.

### Documentation

* Updated root `README.md` and `moltendb-core/README.md` with a **Reserved fields** table documenting `_key`, `_v`,
  `_createdAt`, `_modifiedAt`, and `_expiresAt`, the `_`-prefix enforcement rule, and the always-returned guarantee for
  all five fields. Added TTL documentation covering collection-level TTL via `/schema`, the virtual `_expiresAt` field,
  and the background sweep strategy.
* Expanded TTL documentation in both READMEs with a **sliding-window expiry design decision callout** -- explicitly
  documenting that the TTL clock resets on every insert (not every access), that `/update` does not reset the clock, and
  that collection-level TTL is intentionally designed for ephemeral caches and analytics buffers rather than
  per-document expiry use cases (OTPs, reset tokens, session invalidation). Added a manual per-document expiry pattern
  using `POST /delete` with a `where` clause as the recommended alternative for security-sensitive expiry.
* Expanded `tests/requests_9_ttl.http` with new examples: Â§7 demonstrating that `/update` does not reset the TTL clock,
  Â§8 showing `where` queries on TTL collections, and Â§10a/Â§10b showing the manual per-document expiry pattern for use
  cases not suited to collection-level TTL.

---

# [1.0.0-rc2] (May 12, 2026)

### Breaking Changes

* **Encrypted WAL inner payload switched from JSON to MessagePack** -- existing encrypted `.log` files written by
  `v1.0.0-rc1` or earlier are not readable by this version. Delete or migrate your encrypted WAL before upgrading.

### Performance

* **`Arc<str>` collection-key interning** -- changed the outer `DashMap` key from `String` to `Arc<str>`. During bulk
  insert and WAL replay, all documents in the same collection share a single `Arc<str>` pointer instead of allocating a
  new `String` per document. Saves ~30 B per doc (~30 MB at 1M docs) and reduces allocator pressure during startup.
* **MessagePack in-memory storage** -- switched the hot document map from `serde_json::Value` to `Box<[u8]>` (
  MessagePack bytes). Reduces steady-state RSS for 1M docs from ~4 GB to ~500 MB (~8Ă— lower). Decoding to `Value`
  happens lazily on read; write paths encode via `rmp_serde`. Full analysis in `MEMORY_ANALYSIS.md`.
* **Parallel read paths (rayon)** -- `get_filtered`, `get_all`, and `scan_top_n` now use `rayon` `par_iter` across
  DashMap shards on native targets. MsgPack decode (the dominant cost) runs across all CPU cores. Sequential fallback
  retained for `wasm32`.
* **Bounded per-worker heaps in `scan_top_n`** -- rewrote sort-only paginated queries with rayon `fold` + `reduce` so
  each worker keeps its own small heap (size â‰¤ `cap`). Eliminates the 1M-element intermediate `Vec` that caused ~7s
  latency on sort-only queries; peak intermediate memory drops from O(N) to O(workers Ă— cap).

### Removed (Backward Compatibility)

* **Dropped JSON-lines WAL fallback** -- `log.rs` no longer accepts the legacy JSON-lines log format. The WAL is
  exclusively MessagePack length-prefixed. Databases written before `v1.0.0-rc1` must be migrated or discarded.
* **Dropped legacy snapshot format references** -- `MOLTSNG2` (JSON body) and `MOLTSNAP` (uncompressed) snapshot formats
  were already rejected at load time; all remaining references and comments have been removed. Only `MOLTSNG3` (
  MsgPack + gzip) is supported.

### Bug Fixes

* **Eliminated memory spike during WAL replay and bulk insert** -- changed `apply_entry` to take `LogEntry` by value so
  the `serde_json::Value` tree inside each entry is dropped immediately after MsgPack encoding. Previously both the
  `LogEntry` (~2.4 KB Value tree) and the new `Box<[u8]>` (~120 B) coexisted in RAM for every entry, causing ~2Ă— peak
  memory during boot replay and stress inserts.
* Fixed `compact` method incorrectly placed inside `impl StorageBackend for OpfsStorage` (not a trait member) -- moved
  to a separate `impl OpfsStorage` block in `wasm.rs`.
* Fixed unused-variable warnings (`state`, `hook`) in the default `compact_from_maps` impl under
  `#[cfg(not(feature = "schema"))]`.
* Fixed log file not being cleared after compaction on the encrypted storage path -- `swap_log` now renames the `.tmp`
  file directly on the calling thread when there is no async writer (the case when `EncryptedStorage` delegates
  compaction to its inner storage).
* Implemented `compact_from_maps` on `EncryptedStorage` so the encrypted path now writes snapshots on compaction,
  reducing boot time from O(full WAL) to O(delta entries) -- matching the non-encrypted path behaviour.

---

# [1.0.0-rc1] (May 9, 2026)

### Breaking Changes

* **Removed cold log / tiered storage** -- `my_database.cold.log`, `TieredStorage` hot/cold promotion, `MmapLogReader`,
  and `HOT_TIER_MAX_BYTES` are gone. The `--storage-mode tiered` CLI flag is removed. All data now lives in a single log
  file + snapshot. `TieredStorage` is kept as a thin newtype over `AsyncDiskStorage` for compatibility but does no
  tiering.
* **Removed `--hot-threshold` / `MOLTENDB_HOT_THRESHOLD`** -- the hot/cold eviction threshold no longer exists. All
  documents are loaded into RAM on startup. `evict_collection` and the `evict.rs` module are removed.
* **Removed auto-indexing** -- `indexing.rs`, the `indexes` field on `Db`, `track_query`, and all INDEX log entries are
  removed. Queries always use full-collection scans. Indexes will be rebuilt from scratch in a future release.
* **Removed auto-compaction** -- compaction no longer triggers automatically on write count or log size thresholds. Call
  `POST /snapshot` explicitly to compact. This eliminates surprise I/O spikes during bulk writes and gives full control
  over when the log is reset.

### Performance

* Rewrote `process_get.rs` -- simplified from ~500 lines to ~230 lines; removed incorrect pre-sort fast path, extracted
  `shape_doc` helper, unified sort/truncate/skip pipeline.
* Added bounded heap fast path for `sort + count` queries with no joins or key filters -- only `offset + count`
  documents ever live in RAM regardless of collection size, eliminating the memory-doubling spike previously observed on
  large collections (e.g. top-10 cheapest from 1M docs).
* Fixed snapshot `count` inflation (e.g. 979 000 instead of 100 000) caused by a race where `stream_log_into` replayed
  the old pre-truncation log on top of the already-loaded snapshot. Compaction now synchronously waits for the
  background file swap to complete before returning via a typed `WriterMsg::Compact` enum + condvar.

### Bug Fixes

* Fixed duplicate lines in the cold collection log -- `promote_hot_to_cold` was called with the full database state (
  cold + hot combined) instead of only hot-tier entries. Moot after cold log removal but documented for history.

### Refactor

* Removed `write_compacted_log` from `disk/log.rs` -- only used by the old cold-log promotion path.
* Removed re-exports of `write_compacted_log` and `write_snapshot` from `disk/mod.rs`.
* Deleted stale `my_database.cold.log` from project root.
* Removed stale auto-compaction references from README storage mode descriptions and the "Snapshots, Compaction & Data
  Safety" section.

---

# [1.0.0-rc0] (May 7, 2026)

> âš ď¸Ź **WARNING:** This version was nuked and never made it to the public release.

### Reliability

* Implemented `AtomicBool` circuit breaker in `AsyncDiskStorage` to eliminate silent data loss on background disk I/O
  failure -- when the background flush thread encounters a fatal `writeln!` or `flush` error it sets a shared
  `Arc<AtomicBool>` flag and stops accepting further writes; the core engine checks this flag at the top of every
  `insert`, `update`, and `delete` call and returns `DbError::StorageFault` immediately if it is set, preventing the
  in-memory state from diverging from what is persisted on disk
* Mapped `DbError::StorageFault` to `HTTP 503 Service Unavailable` in `process_set.rs` -- clients now receive an
  explicit error response instead of a false `200 OK` when the storage layer is in a faulted state

### Performance

* Removed intermediate `Vec<LogEntry>` from snapshot loading path -- entries are now streamed directly into the
  in-memory `DashMap` as they are read from disk, halving peak startup RAM usage for large snapshots (previously `~2Ă—`
  snapshot file size, now `~1Ă—`)
* Snapshot files are now gzip-compressed using `flate2` (pure Rust, WASM-compatible); typical JSON snapshots compress
  3Ă—-8Ă—, significantly reducing disk usage and improving startup I/O on large datasets; magic header updated to
  `MOLTSNG2` for forward/backward compatibility -- old `MOLTSNAP` snapshots are gracefully ignored and state is rebuilt
  from the WAL
* Added lazy `Db::get_filtered()` operation that filters documents *while* iterating the collection `DashMap`, only
  cloning matches; replaces the prior `get_all` -> filter pattern in `process_get` for queries with WHERE but no joins.
  Reduces peak memory and time on sparse WHERE queries from O(total) to O(matches) -- e.g. a query returning 84 matches
  from 1M docs no longer clones the other 999 916 documents
* Added bounded top-N streaming via `Db::scan_top_n()` -- for `sort + count` (and optional `offset`) queries with no
  joins, documents are streamed directly into a max-heap of capacity `offset + count` with a peek-before-clone guard so
  docs that cannot beat the current worst candidate are never cloned. Peak memory is now O(offset + count) instead of O(
  matching_docs); eliminates the memory-doubling spike previously observed on Â§6 stress queries and brings response
  times under 1s on 1M-doc collections

### Reliability

* Replaced `.unwrap()` on disk I/O in the async storage background flush thread (`async_storage.rs`) with `match`/
  `if let Err` blocks that log errors via `tracing::error!` and return early -- prevents silent panics on disk-full or
  lost file handle conditions
* Replaced `.lock().unwrap()` on the WASM handle mutex in `wasm.rs` (6 call sites) with
  `.lock().expect("db handle mutex poisoned")` -- provides a clear, descriptive panic message if the mutex is ever
  poisoned by a prior panic

### Refactor

* Grouped the 8 parameters of `insert()` into `InsertParams<'a>` struct (`insert.rs`) to resolve Clippy's "too many
  arguments" warning; re-exported from `operations/mod.rs`; all call sites updated
* Grouped the 8 parameters of `update()` into `UpdateParams<'a>` struct (`update.rs`) for the same reason; re-exported
  from `operations/mod.rs`; all call sites updated

### Code Quality

* Fixed all ~44 Clippy warnings in `moltendb-core`: collapsed nested `if` statements, replaced redundant closures with
  direct constructor references (`DbError::Serialization`), removed unnecessary `Ok(â€¦?)` in `encrypted.rs`, replaced
  `or_insert_with` with `or_default`, replaced manual `% 2 == 0` checks with `.is_multiple_of()`, replaced manual suffix
  stripping with `.strip_suffix()`
* Fixed all Clippy warnings in `moltendb-server`, `moltendb-auth`, and `moltendb-wasm`: `or_insert_with`, redundant
  cast, collapsible `if`, unnecessary `as_ref`/deref/borrow, and doc comment indentation issues
* Added `#![deny(warnings)]` to all four crates (`moltendb-core/src/lib.rs`, `moltendb-auth/src/lib.rs`,
  `moltendb-wasm/src/lib.rs`, `moltendb-server/src/main.rs`) -- future warnings are now hard compile errors

### Documentation

* Updated all README files to reflect `1.0.0-rc` release candidate status: replaced `âš ď¸Ź Beta Software` notice with
  `đźš€ Release Candidate (v1.0.0-rc)`, added `status-1.0.0-rc` badge to all six Rust crate READMEs, corrected test
  count badges (`88 passing` root, `59 passing` server), and updated `## Current limitations` heading in
  `moltendb-auth/README.md` from `v0.10.3` to `v1.0.0-rc`

# [0.10.3] (May 6, 2026)

### Bug Fixes

* Fixed stale document state after log replay: documents were not correctly removed from in-memory state during startup
  replay when a `DELETE` entry was encountered for a previously cold (disk-pointer) document

# [0.10.2] (May 4, 2026)

### Refactor

* Extracted `Db::open()` (native) and `Db::open_wasm()` (WASM) from `engine/mod.rs` into dedicated files
  `engine/open.rs` and `engine/open_wasm.rs`; `engine/mod.rs` now only declares and delegates
* Removed duplicate single-key `get` method; renamed `get_batch` to `get` -- callers now pass `Vec<String>` and receive
  `HashMap<String, Value>`; all call sites and tests updated
* Removed duplicate single-key `delete` method; renamed `delete_batch` to `delete` -- callers now pass `Vec<String>`;
  all call sites and tests updated
* Renamed `insert_batch` to `insert` across the entire codebase for consistency with the new `get`/`delete` naming
* Moved `compact`, `evict_collection`, and `recover_to` implementations from `engine/mod.rs` into dedicated files
  `operations/compact.rs`, `operations/evict.rs`, and `operations/recover.rs`; `engine/mod.rs` is now a thin delegation
  layer

# [0.10.1] (May 4, 2026)

Yanked due to a build issue.

# [0.10.0] (May 1, 2026)

### Features

* WebSocket JWT scope filtering -- each connected client only receives change events for collections their token's
  scopes grant `read` access to; admin tokens (`*:*:*`) receive all events
* WebSocket revocation enforcement at connection time -- revoked tokens are rejected immediately with a structured
  error (`{"error":"token_revoked", "detail":"..."}`) before the connection is accepted
* WebSocket revocation re-check on open connections -- a background ticker checks every 30 seconds whether the
  authenticated token has been revoked since the connection was opened; if so, the client receives a `token_revoked`
  error and the connection is closed
* Distinct WebSocket auth error codes -- each failure mode now returns a specific `error` code: `invalid_message`,
  `invalid_action`, `missing_token`, `invalid_token`, `token_revoked`
* Broadcast lag observability -- `RecvError::Lagged` events are now logged as warnings instead of silently dropping the
  connection
* Configurable bind host -- new `--host` CLI flag and `MOLTENDB_HOST` env var (default `0.0.0.0`); supports any
  IPv4/IPv6 address, enabling Docker and multi-interface deployments without recompilation
* In-memory mode -- new `--in-memory` CLI flag and `MOLTENDB_IN_MEMORY` env var; bypasses the WAL and all disk I/O
  entirely, turning MoltenDB into a pure RAM cache (Redis-like); compaction and revocation-file persistence are
  automatically skipped; a startup warning is emitted to make the ephemeral nature explicit
* WASM in-memory mode -- `WorkerDb.create()` now accepts an `in_memory` boolean as its ninth parameter; when `true`,
  OPFS is never opened and all data lives only in the browser's RAM -- useful for ephemeral session caches or testing
  without touching persistent storage

# [0.9.0] (Apr 30, 2026)

### Features

* configurable max keys per request for core and wasm engines (`max_keys_per_request` in `DbConfig`,
  `--max-keys-per-request` CLI flag / `MOLTENDB_MAX_KEYS_PER_REQUEST` env var, `maxKeysPerRequest` param in
  `MoltenDb.create()`)
* dev mode: `--dev-mode` flag / `MOLTENDB_DEV_MODE` env var -- runs the server over plain HTTP/WS instead of HTTPS/WSS,
  ignoring `--cert` and `--key` (for local development only)
* telemetry endpoints: `GET /system/health` (public liveness check) and `GET /system/metrics` (admin-only snapshot of
  uptime, process memory, host RAM/disk, and database internals -- `hot_keys_count`, `hot_tier_threshold`,
  `wal_size_bytes`, `storage_mode`)

# [0.8.0](https://github.com/maximilian27/MoltenDB/compare/v0.7.0...v0.8.0) (Apr 30, 2026)

### Bug Fixes

* wasm error ([44478a9](https://github.com/maximilian27/MoltenDB/commit/44478a9f6060303b6091f46f1ceb36bfd1eeafe0))

### Features

* revoke token ([a96fb32](https://github.com/maximilian27/MoltenDB/commit/a96fb320aa735a9c0a1437fb56375e3cef18e59c))
* role based
  privileges ([8b535af](https://github.com/maximilian27/MoltenDB/commit/8b535afbfe5781803f2903977739fa6dcde7dd7d))

# [0.7.0](https://github.com/maximilian27/MoltenDB/compare/v0.6.3...v0.7.0) (Apr 28, 2026)

### Bug Fixes

* changelog merge
  conflict ([9f8a3fc](https://github.com/maximilian27/MoltenDB/commit/9f8a3fce25f1eb8e4fe3ab8cdc1effaaa8b3911e))

### Features

* post backup hook ([99f95e7](https://github.com/maximilian27/MoltenDB/commit/99f95e7b4f90025b7ff5f7a385a62b9bc467ec6e))

## [0.6.3](https://github.com/maximilian27/MoltenDB/compare/v0.6.2...v0.6.3) (Apr 28, 2026)

### Features

* post backup hook ([e3b0f55](https://github.com/maximilian27/MoltenDB/commit/e3b0f55dca219a9b4b0c5dc59f1dd2e0155949ac))

## [0.6.2](https://github.com/maximilian27/MoltenDB/compare/v0.4.0...v0.6.2) (Apr 27, 2026)

### Bug Fixes

* add transactions to
  update ([0c4681d](https://github.com/maximilian27/MoltenDB/commit/0c4681d8b4bec2e89bf81a0cefe7fb5c30db0bb5))
* fix disk log
  issues ([7b9c7a0](https://github.com/maximilian27/MoltenDB/commit/7b9c7a03ddb6a488de8265177550e67bd272cfae))
* load snapshot in
  memory ([a7b6242](https://github.com/maximilian27/MoltenDB/commit/a7b62427564f6992145b18f982ec4b3cf100a468))
* persist delete functionality across
  restart ([d21f56c](https://github.com/maximilian27/MoltenDB/commit/d21f56ca621ecbfd631c1912f646cb0e64904a4b))
* wasm compilation
  issues ([cf7f2fd](https://github.com/maximilian27/MoltenDB/commit/cf7f2fd4b3acb63574f9deb956b8c0677be233c9))

### Features

* add timestamp
  metadata ([65b7363](https://github.com/maximilian27/MoltenDB/commit/65b73634189297ad0a55028608e7642d34d57005))
* expose snapshot
  endpoint ([f8a9b09](https://github.com/maximilian27/MoltenDB/commit/f8a9b091331dcddc91ab6c8e27b381dc5d08c1cd))
* point in time
  recovery ([619239d](https://github.com/maximilian27/MoltenDB/commit/619239db6597be4ee945fcd7caa8812ebcd4007a))
* schema
  validation ([85757a2](https://github.com/maximilian27/MoltenDB/commit/85757a2f0f959dbe6bf9513ea5fb657c2acd97bf))
* versioned snapshot and backup
  management ([0a46217](https://github.com/maximilian27/MoltenDB/commit/0a46217ac6dc06686b1f2760fc3ab290be969ffc))
* WAL transaction
  markers ([51f7ecd](https://github.com/maximilian27/MoltenDB/commit/51f7ecd04c42bbbda5412601807856154f9ba32e))

# [0.4.0](https://github.com/maximilian27/MoltenDB/compare/v0.3.0-beta.4...v0.4.0) (Apr 23, 2026)

### Bug Fixes

* wasm module
  build ([ddcae13](https://github.com/maximilian27/MoltenDB/commit/ddcae139cacd8be7fbef23ba0701ebbfc95c0cad))

### Features

* configurable hot
  threshold ([f1a38fb](https://github.com/maximilian27/MoltenDB/commit/f1a38fb432ebb49f3942fd9000fd5b8a0c15714e))
* expose hotThreshold, encryptionKey, writeMode, rateLimitRequests, rateLimitWindow and maxBodySize to web
  packages ([7450274](https://github.com/maximilian27/MoltenDB/commit/7450274769b6c9370ddfab9a999d1244be8647e4))
* oom protection ([db4cc96](https://github.com/maximilian27/MoltenDB/commit/db4cc96971c3378a6a656520b994d061adf33862))




