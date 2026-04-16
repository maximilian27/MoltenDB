// ─── handlers.rs ──────────────────────────────────────────────────────────────
// This file contains the business logic for each database operation exposed
// via the HTTP API. Each `process_*` function:
//   1. Validates the incoming JSON payload.
//   2. Reads parameters from the payload.
//   3. Calls the appropriate Db method(s).
//   4. Returns a JSON response value.
//
// These functions are called by the thin Axum handler wrappers in main.rs,
// and also by the WebSocket handler for WS-based operations.
//
// The separation between main.rs (HTTP wiring) and handlers.rs (logic) means
// the same logic can be reused from both HTTP and WebSocket contexts.
// ─────────────────────────────────────────────────────────────────────────────

// process_analytics uses server-only deps so it stays native-only.
// The other four modules are pure logic and compile for WASM too.

pub mod process_get;
pub mod process_set;
pub mod process_update;
pub mod process_delete;
#[cfg(not(target_arch = "wasm32"))]
pub mod process_analytics;

pub use process_get::process_get;
pub use process_set::process_set;
pub use process_update::process_update;
pub use process_delete::process_delete;
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)]
pub use process_analytics::process_analytics;

