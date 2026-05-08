use dashmap::DashMap;
use tracing::info;
use crate::engine::types::{DbError, LogEntry};
use crate::engine::storage::StorageBackend;

pub fn compact(
    state: &DashMap<String, DashMap<String, serde_json::Value>>,
    #[cfg(feature = "schema")]
    schemas: &DashMap<String, std::sync::Arc<(serde_json::Value, jsonschema::Validator)>>,
    storage: &dyn StorageBackend,
    post_backup_script: Option<String>,
) -> Result<Vec<LogEntry>, DbError> {
    info!("🔨 Starting Log Compaction...");

    let mut entries = Vec::new();

    // One INSERT per live document across all collections.
    for col_ref in state.iter() {
        let col_name = col_ref.key();
        for item_ref in col_ref.value().iter() {
            let entry = LogEntry::new(
                "INSERT".to_string(),
                col_name.clone(),
                item_ref.key().clone(),
                item_ref.value().clone(),
            );
            entries.push(entry);
        }
    }

    // One SCHEMA entry per collection.
    #[cfg(feature = "schema")]
    for schema_ref in schemas.iter() {
        let col_name = schema_ref.key();
        let (schema_json, _) = &**schema_ref.value();
        entries.push(LogEntry::new(
            "SCHEMA".to_string(),
            col_name.clone(),
            "".to_string(),
            schema_json.clone(),
        ));
    }

    // Delegate the actual file rewrite (and snapshot write) to the storage backend.
    storage.compact_with_hook(entries.clone(), post_backup_script)?;

    info!("✅ Log Compaction Finished!");
    Ok(entries)
}
