use crate::engine::StorageBackend;
use dashmap::DashMap;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

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
    pub seq_index: &'a DashMap<Arc<str>, Arc<RwLock<BTreeMap<u64, String>>>>,
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
