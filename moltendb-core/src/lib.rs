// moltendb-core — pure engine, no HTTP, no auth, no rate limiting.
pub mod engine;
pub mod query;
pub mod analytics;
pub mod validation;
pub mod handlers;

// WASM web worker entry point — only compiled for wasm32 target
#[cfg(target_arch = "wasm32")]
pub mod worker;

#[cfg(target_arch = "wasm32")]
pub use worker::*;

