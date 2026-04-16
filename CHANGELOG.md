# [0.2.0-beta.1] (2026-04-16)


### Architecture

* **Cargo Workspace refactor** — MoltenDB is now a 3-crate workspace. The monolithic `src/` has been split into three independent, composable crates:
  * `moltendb-core` — pure engine with zero HTTP/auth dependencies. Compiles to native `rlib`/`cdylib` for embedding and to WASM via `wasm-pack`. Can be used as a standalone embedded database in any Rust project.
  * `moltendb-auth` — identity layer (Argon2 password hashing, JWT minting/validation, `UserStore`). Depends only on `moltendb-core`.
  * `moltendb-server` — network layer (Axum, TLS, CORS, rate limiting, CLI config). Depends on both `moltendb-core` and `moltendb-auth`.
* Integration tests moved to `moltendb-server/tests/integration.rs`
* WASM build now targets `moltendb-core` only: `wasm-pack build moltendb-core --target web`
* Binary install command updated: `cargo install moltendb-server`


### Security

* **JWT secret is now mandatory** — the server refuses to start if `--jwt-secret` / `JWT_SECRET` is not set (previously fell back to a hardcoded dev string)
* **CORS origin is now configurable** — `--cors-origin` / `CORS_ORIGIN` flag added (default `*` for dev; set to your frontend URL in production, comma-separated for multiple origins)
* **HTTP-layer body size limit** — `RequestBodyLimitLayer` added at the router level; configurable via `--max-body-size` / `MAX_BODY_SIZE` (default 10 MB). Oversized requests are now rejected before application code sees them.
* **Single-user mode documented** — `add_user` dead code removed from `UserStore`; v1 supports exactly one admin user configured at startup


# [0.1.0-beta.1] (2026-04-08)


### Bug Fixes

* **AsyncDiskStorage**: store writer task `JoinHandle` and implement `Drop` to await full queue drain and flush before process exit — fixes data loss on graceful shutdown where buffered log entries were silently discarded


### Features

* case-insensitive string matching across all query operators (`$eq`, `$ne`, `$contains`, `$in`, `$nin`)
* stress tooling: `examples/generate_stress_data.rs`, `examples/stress_insert.rs`, `examples/stress_fetch.rs` for real-world load testing against a live server



# [0.1.0-alpha.25](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-alpha.23...v0.1.0-alpha.25) (2026-04-07)


### Performance Improvements

* dev only deps for reqwest ([9910666](https://github.com/maximilian27/MoltenDB/commit/9910666cca2de2e31d67515813d01b1299ca47fb))



# [0.1.0-alpha.23](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-alpha.21...v0.1.0-alpha.23) (2026-04-06)



# [0.1.0-alpha.21](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-alpha.20...v0.1.0-alpha.21) (2026-03-27)


### Features

* wasm module auto-compaction improvements and logging ([aa80156](https://github.com/maximilian27/MoltenDB/commit/aa801566655f21cc94604eb8f47635a8e7cbd265))



# [0.1.0-alpha.20](https://github.com/maximilian27/MoltenDB/compare/v0.1.0-alpha.16...v0.1.0-alpha.20) (2026-03-21)


### Bug Fixes

* generate changelog configuration ([4afb758](https://github.com/maximilian27/MoltenDB/commit/4afb758adb3c3c151fd7aa88825b9438fde417b7))
* remove skip ci in order to trigger next workflows ([b5a81d0](https://github.com/maximilian27/MoltenDB/commit/b5a81d0346beff15c3a33e0e0d8e46eb98b7200a))
* update changelog generation ([261e533](https://github.com/maximilian27/MoltenDB/commit/261e533ff7c2f96fbbb85fe9631cb7417b16e460))


### Features

* wire up native Rust changefeed to JS query builder ([ca433f3](https://github.com/maximilian27/MoltenDB/commit/ca433f3f12f4a871f24eddaff8e0df3567102b47))



