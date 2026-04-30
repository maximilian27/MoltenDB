## [0.8.0](https://github.com/maximilian27/MoltenDB/compare/v0.7.0...v0.8.0) (2026-04-30)
* **auth:** persistent token revocation — revoked JTIs are written to `<db_name>.revocations.json` immediately on `DELETE /auth/tokens/:jti` and reloaded on server startup so revocations survive restarts; background prune task also saves the file every 60 seconds (`RevocationsPath` newtype, `save_to_file`, `load_from_file` in `moltendb-auth`)
### Features
* **auth:** added `scopes` field to JWT `Claims` with `has_access(action, collection, key)`, `has_collection_access`, and `is_admin` helpers for document-level permission checks
* **auth:** added `create_scoped_token(username, scopes, ttl_secs)` — root `create_token` now delegates to this internally with the `admin` scope and a 24-hour TTL
* **auth:** added `POST /auth/delegate` endpoint (admin-only) that mints narrow-scoped JWTs for clients; accepts `client_id`, `scopes`, and optional `ttl_secs`; validates scope format before signing
* **auth:** added `DelegateRequest` and `DelegateResponse` types to `moltendb-auth` for easy integration with any external auth system
* **auth:** enforced scope checks on every protected endpoint — `POST /set` and `POST /update` require `write:{collection}:*`; `POST /get` and `GET /collections/{col}` require `read:{collection}:*`; `GET /collections/{col}/docs/{key}` requires `read:{collection}:{key}` (document-level tokens now work correctly); `POST /delete` requires `delete:{collection}:*`; `POST /snapshot` requires `admin`
* **auth:** `moltendb-auth` crate is now explicitly excluded from WASM compilation — `#![cfg(not(target_arch = "wasm32"))]` gates the entire crate and all dependencies are under `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`
* **auth:** added `jti` (UUID) field to `Claims` — every token minted by `create_scoped_token` now carries a unique JWT ID for revocation tracking; legacy tokens without `jti` deserialize safely via `#[serde(default)]`
* **auth:** added `RevocationStore` — an in-memory `DashMap<String, Instant>` of revoked JTIs; entries are pruned automatically by a background task every 60 seconds once the token's original TTL has passed
* **auth:** added `DELETE /auth/tokens/:jti` endpoint (admin-only) — accepts `{ "exp": <unix_timestamp> }` to set the prune deadline; revoked tokens are immediately rejected by `auth_middleware` with `401 Unauthorized`
* **auth:** `auth_middleware` now checks the `RevocationStore` extension on every request — revoked tokens are rejected even if their signature and expiry are still valid

### Breaking Changes
* **auth:** existing JWTs issued before this release have no `scopes` field — they will deserialize with `scopes: []` (via `#[serde(default)]`) and will be rejected by all scope-aware endpoints; re-issue tokens after upgrading
* **auth:** all write/delete/query endpoints now return `403 Forbidden` if the token lacks the required scope — previously they were scope-blind and returned data regardless of token permissions

## [0.7.0](https://github.com/maximilian27/MoltenDB/compare/v0.6.3...v0.7.0) (2026-04-28)
### Features
* **backups:** added script-based post-backup hook with automatic PowerShell ExecutionPolicy bypass on Windows
* **backups:** switched from arbitrary shell commands to dedicated script execution for improved security

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



# [0.3.0-beta.4](https://github.com/maximilian27/MoltenDB/compare/v0.2.0-beta.4...v0.3.0-beta.4) (2026-04-17)


### Bug Fixes

* missing import ([6c47a3b](https://github.com/maximilian27/MoltenDB/commit/6c47a3b702f73b297c850c7cda1b2952744c0d8e))
* missing import ([2b3e531](https://github.com/maximilian27/MoltenDB/commit/2b3e53171ad173ca6448affd800c796b480fa7b1))



