## [0.7.0](https://github.com/maximilian27/MoltenDB/compare/v0.6.3...v0.7.0) (2026-04-28)
### Features
* **backups:** added script-based post-backup hook with automatic PowerShell ExecutionPolicy bypass on Windows
* **backups:** switched from arbitrary shell commands to dedicated script execution for improved security

# [0.6.3](https://github.com/maximilian27/MoltenDB/compare/v0.6.2...v0.6.3) (2026-04-28)
### Features
* **configuration:** consolidated database initialization into `DbConfig` struct for improved maintainability
* **naming:** standardized all environment variables with `MOLTENDB_` prefix

### Breaking Changes
* **identity:** renamed `--admin-user` to `--root-user` and `--admin-password` to `--root-password` (env vars also updated to `MOLTENDB_ROOT_USER` and `MOLTENDB_ROOT_PASSWORD`)
* **configuration:** `Db::open` and `Db::open_wasm` now accept a `DbConfig` struct instead of individual arguments

# [0.6.3](https://github.com/maximilian27/MoltenDB/compare/v0.6.2...v0.6.3) (2026-04-28)

### Features

* **backups:** added script-based post-backup hook with automatic PowerShell ExecutionPolicy bypass on Windows
* **backups:** switched from arbitrary shell commands to dedicated script execution for improved security

# [0.6.2](https://github.com/maximilian27/MoltenDB/compare/v0.4.0...v0.6.2) (2026-04-27)
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



# [0.2.0-beta.4](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-beta.2...v0.2.0-beta.4) (2026-04-16)


### Bug Fixes

* wasm module ([fc4101c](https://github.com/maximilian27/MoltenDB/commit/fc4101c15a99079cc0fff0abc045e45c93fd8b41))



