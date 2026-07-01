// ─── operations/mod.rs ────────────────────────────────────────────────────────
// This module contains the core CRUD (Create, Read, Update, Delete) operations
// that mutate or query the in-memory database state.
//
// Every function here follows the same pattern:
//   1. Mutate the in-memory DashMap (instant, in RAM).
//   2. Write a LogEntry to the storage backend (persisted to disk/OPFS).
//   3. Broadcast a change event over the Tokio broadcast channel (for WebSocket
//      subscribers who want real-time notifications).
//
// These functions are called by the Db methods in engine/mod.rs, which in turn
// are called by the HTTP handlers in handlers.rs and the WASM worker in worker.rs.
//
// Why separate operations from mod.rs?
//   mod.rs defines the Db struct and its public API. operations contains the
//   actual implementation logic. This keeps mod.rs clean and makes the
//   individual operations easy to find and reason about.
// ─────────────────────────────────────────────────────────────────────────────

mod common;
mod compact;
mod delete;
mod get;
#[cfg(not(target_arch = "wasm32"))]
mod recover;
mod set;
pub mod ttl;
pub mod types;
mod update;

pub use compact::compact;
pub use delete::{delete, delete_collection, delete_filtered, evict_oldest};
pub use get::{get, get_all, get_filtered, scan_top_n, scan_top_n_raw};
#[cfg(not(target_arch = "wasm32"))]
pub use recover::recover_to;
pub use set::insert;
pub use types::{InsertParams, UpdateParams};
pub use update::update;
