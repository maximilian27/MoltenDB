use dashmap::DashMap;
use std::sync::Arc;
use tracing::info;
use crate::engine::types::DbError;
use crate::engine::storage::StorageBackend;

pub fn compact(
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    #[cfg(feature = "schema")]
    schemas: &DashMap<String, std::sync::Arc<(serde_json::Value, jsonschema::Validator)>>,
    storage: &dyn StorageBackend,
) -> Result<(), DbError> {
    info!("🔨 Starting Log Compaction...");

    #[cfg(not(feature = "schema"))]
    storage.compact_from_maps(state)?;

    #[cfg(feature = "schema")]
    storage.compact_from_maps(state, schemas)?;

    info!("✅ Log Compaction Finished!");
    Ok(())
}
