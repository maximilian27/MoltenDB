use dashmap::DashMap;
use tracing::info;
use crate::engine::types::DbError;
use crate::engine::storage::StorageBackend;

pub fn compact(
    state: &DashMap<String, DashMap<String, Box<[u8]>>>,
    #[cfg(feature = "schema")]
    schemas: &DashMap<String, std::sync::Arc<(serde_json::Value, jsonschema::Validator)>>,
    storage: &dyn StorageBackend,
    post_backup_script: Option<String>,
) -> Result<(), DbError> {
    info!("🔨 Starting Log Compaction...");

    #[cfg(not(feature = "schema"))]
    storage.compact_from_maps(state, post_backup_script)?;

    #[cfg(feature = "schema")]
    storage.compact_from_maps(state, schemas, post_backup_script)?;

    info!("✅ Log Compaction Finished!");
    Ok(())
}
