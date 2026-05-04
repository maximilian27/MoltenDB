// ─── operations/compact.rs ────────────────────────────────────────────────────
// Compacts the log file — rewrites it to contain only the current live state.
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::DashMap;
use tracing::info;
use crate::engine::types::{DbError, LogEntry};
use crate::engine::storage::StorageBackend;

pub fn compact(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    #[cfg(feature = "schema")]
    schemas: &DashMap<String, std::sync::Arc<(serde_json::Value, jsonschema::Validator)>>,
    indexes: &DashMap<String, DashMap<String, dashmap::DashSet<String>>>,
    storage: &dyn StorageBackend,
    post_backup_script: Option<String>,
) -> Result<Vec<LogEntry>, DbError> {
    info!("🔨 Starting Log Compaction...");

    let mut entries = Vec::new();

    // One INSERT per live document across all collections.
    for col_ref in state.iter() {
        let col_name = col_ref.key();
        for item_ref in col_ref.value().iter() {
            let entry = match item_ref.value() {
                crate::engine::types::DocumentState::Hot(v) => {
                    LogEntry::new(
                        "INSERT".to_string(),
                        col_name.clone(),
                        item_ref.key().clone(),
                        v.clone(),
                    )
                }
                crate::engine::types::DocumentState::Cold(ptr) => {
                    let bytes = storage.read_at(ptr.offset, ptr.length)?;
                    serde_json::from_slice(&bytes)?
                }
            };
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

    // One INDEX entry per registered index.
    for index_ref in indexes.iter() {
        let parts: Vec<&str> = index_ref.key().split(':').collect();
        if parts.len() == 2 {
            entries.push(LogEntry::new(
                "INDEX".to_string(),
                parts[0].to_string(),
                parts[1].to_string(),
                serde_json::json!(null),
            ));
        }
    }

    // Delegate the actual file rewrite (and snapshot write) to the storage backend.
    storage.compact_with_hook(entries.clone(), post_backup_script)?;

    info!("✅ Log Compaction Finished!");
    Ok(entries)
}
