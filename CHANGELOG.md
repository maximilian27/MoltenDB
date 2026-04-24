# [0.6.0](https://github.com/maximilian27/MoltenDB/compare/v0.5.0...v0.6.0) (2026-04-24)

### Features

* **Point-in-Time Recovery (PITR):** Added a standalone `recover` CLI subcommand to restore the database to a specific millisecond or sequence number.
* **Engine-level Timestamps (`_t`):** Every log entry (INSERT, DELETE, DROP, etc.) now includes an immutable `_t` Unix timestamp in milliseconds for precise recovery and auditing.
* **Snapshot Versioning:** Implemented automatic snapshot rotation where old snapshots are moved to a `/backup` folder before being replaced, preventing data loss during compaction.
* **On-demand Snapshots:** Added a new `POST /snapshot` API endpoint (authenticated) to trigger manual database snapshots.
* **Robust Log Streaming:** Updated the `StorageBackend` trait to support early-exit during log replay, enabling efficient partial restores.

### Bug Fixes

* **WASM Feature Gating:** Fixed compilation errors in the WASM build by properly gating native storage backends and resolving unresolved imports.
* **Database Initialization:** Improved `Db::open` to automatically create parent directories if they don't exist, preventing `NotFound` errors on new environments.

# [0.5.0](https://github.com/maximilian27/MoltenDB/compare/v0.4.0...v0.5.0) (2026-04-24)


### Features

* schema validation ([85757a2](https://github.com/maximilian27/MoltenDB/commit/85757a2f0f959dbe6bf9513ea5fb657c2acd97bf))
* WAL transaction markers ([51f7ecd](https://github.com/maximilian27/MoltenDB/commit/51f7ecd04c42bbbda5412601807856154f9ba32e))



# [0.4.0](https://github.com/maximilian27/MoltenDB/compare/v0.3.0-beta.4...v0.4.0) (2026-04-23)


### Bug Fixes

* wasm module build ([ddcae13](https://github.com/maximilian27/MoltenDB/commit/ddcae139cacd8be7fbef23ba0701ebbfc95c0cad))


### Features

* configurable hot threshold ([f1a38fb](https://github.com/maximilian27/MoltenDB/commit/f1a38fb432ebb49f3942fd9000fd5b8a0c15714e))
* expose hotThreshold, encryptionKey, writeMode, rateLimitRequests, rateLimitWindow and maxBodySize to web packages ([7450274](https://github.com/maximilian27/MoltenDB/commit/7450274769b6c9370ddfab9a999d1244be8647e4))
* oom protection ([db4cc96](https://github.com/maximilian27/MoltenDB/commit/db4cc96971c3378a6a656520b994d061adf33862))



# [0.3.0-beta.4](https://github.com/maximilian27/MoltenDB/compare/v0.2.0-beta.4...v0.3.0-beta.4) (2026-04-17)


### Bug Fixes

* missing import ([6c47a3b](https://github.com/maximilian27/MoltenDB/commit/6c47a3b702f73b297c850c7cda1b2952744c0d8e))
* missing import ([2b3e531](https://github.com/maximilian27/MoltenDB/commit/2b3e53171ad173ca6448affd800c796b480fa7b1))



# [0.2.0-beta.4](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-beta.2...v0.2.0-beta.4) (2026-04-16)


### Bug Fixes

* wasm module ([fc4101c](https://github.com/maximilian27/MoltenDB/commit/fc4101c15a99079cc0fff0abc045e45c93fd8b41))



# [0.1.0-beta.2](https://github.com/maximilian27/MoltenDB/compare/758c824a16a51f729a383011c8e8f60368bf3859...v0.1.0-beta.2) (2026-04-08)


### Bug Fixes

* async writer data loss on graceful shutdown ([5dd2514](https://github.com/maximilian27/MoltenDB/commit/5dd25141336924d3269965350c841b9b166016a2))
* bump version for cargo lock ([d98ac29](https://github.com/maximilian27/MoltenDB/commit/d98ac29ab629eaf1f844e7b954f9a51bf45f9f2e))
* generate changelog configuration ([4afb758](https://github.com/maximilian27/MoltenDB/commit/4afb758adb3c3c151fd7aa88825b9438fde417b7))
* remove skip ci in order to trigger next workflows ([b5a81d0](https://github.com/maximilian27/MoltenDB/commit/b5a81d0346beff15c3a33e0e0d8e46eb98b7200a))
* update changelog generation ([261e533](https://github.com/maximilian27/MoltenDB/commit/261e533ff7c2f96fbbb85fe9631cb7417b16e460))


### Features

* auto bump alpha version ([67bd2d0](https://github.com/maximilian27/MoltenDB/commit/67bd2d04d469e8b8fddfff1d7f4b9d12baf7bad6))
* auto tag on master ([0a86299](https://github.com/maximilian27/MoltenDB/commit/0a86299dff782f9fa4eae04cf14e5f1dd920acf4))
* build and release workflow ([dcbad5b](https://github.com/maximilian27/MoltenDB/commit/dcbad5b1bb89abd759ebb7f942b5bf9d0f90a8ef))
* build wasm and sync to web repo with GitHub Actions ([03e309b](https://github.com/maximilian27/MoltenDB/commit/03e309bd6d1b3cc1737ac969a867ce622a652563))
* MoltenDB v0.1.0-alpha ([758c824](https://github.com/maximilian27/MoltenDB/commit/758c824a16a51f729a383011c8e8f60368bf3859))
* wasm module auto-compaction improvements and logging ([aa80156](https://github.com/maximilian27/MoltenDB/commit/aa801566655f21cc94604eb8f47635a8e7cbd265))
* wire up native Rust changefeed to JS query builder ([ca433f3](https://github.com/maximilian27/MoltenDB/commit/ca433f3f12f4a871f24eddaff8e0df3567102b47))


### Performance Improvements

* dev only deps for reqwest ([9910666](https://github.com/maximilian27/MoltenDB/commit/9910666cca2de2e31d67515813d01b1299ca47fb))



