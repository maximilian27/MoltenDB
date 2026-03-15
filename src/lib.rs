// 1. We expose only the pure logic (No Axum, no Auth, no Rate Limiters!)
pub mod engine;
pub mod query;
pub mod validation;
pub mod analytics;
pub mod handlers;

// 2. We expose the Javascript API bridge only when compiling for the web

#[cfg(target_arch = "wasm32")]
pub mod worker;

#[cfg(target_arch = "wasm32")]
pub use worker::*;