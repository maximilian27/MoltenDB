// ─── operations/document_processing ─────────────────────────────────────────────────────
// Shared utilities used across all operation modules.
// ─────────────────────────────────────────────────────────────────────────────

use crate::engine::StorageBackend;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// Returns the current time as Unix milliseconds (u64).
///
/// Used to stamp `_createdAt` and `_modifiedAt` on every document write.
/// Uses `web-time` for WASM compatibility, `std::time` on native.
pub fn now_unix_ms() -> u64 {
    use web_time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Parameters for the [`insert`] operation.
///
/// Grouping these into a struct keeps the function signature within Clippy's
/// argument-count limit and makes call sites more readable.
pub struct InsertParams<'a> {
    pub state: &'a DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    pub storage: &'a Arc<dyn StorageBackend>,
    pub tx: &'a tokio::sync::broadcast::Sender<String>,
    #[cfg(feature = "schema")]
    pub schemas: &'a DashMap<String, Arc<(Value, jsonschema::Validator)>>,
    pub seq_counters: &'a DashMap<String, AtomicU64>,
    pub collection: &'a str,
    pub items: Vec<(String, Value)>,
}

/// Parameters for the [`update`] operation.
///
/// Grouping these into a struct keeps the function signature within Clippy's
/// argument-count limit and makes call sites more readable.
pub struct UpdateParams<'a> {
    pub state: &'a DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    pub storage: &'a Arc<dyn StorageBackend>,
    pub tx: &'a tokio::sync::broadcast::Sender<String>,
    #[cfg(feature = "schema")]
    pub schemas: &'a DashMap<String, Arc<(Value, jsonschema::Validator)>>,
    pub collection: &'a str,
    pub key: &'a str,
    pub updates: Value,
}
