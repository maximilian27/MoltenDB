// ─── operations/recover.rs ────────────────────────────────────────────────────
// Recovers the database state to a specific point in time or sequence number.
// Used by the CLI for Point-in-Time Recovery (PITR).
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
use std::ops::ControlFlow;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use dashmap::{DashMap, DashSet};
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::types::{DbError, LogEntry};
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::storage::StorageBackend;

#[cfg(not(target_arch = "wasm32"))]
pub fn recover_to(
    storage: &dyn StorageBackend,
    to_time: Option<u64>,
    to_seq: Option<u64>,
) -> Result<Vec<LogEntry>, DbError> {
    let state: DashMap<String, DashMap<String, crate::engine::types::DocumentState>> = DashMap::new();
    let indexes: DashMap<String, DashMap<String, DashSet<String>>> = DashMap::new();
    #[cfg(feature = "schema")]
    let schemas: DashMap<String, Arc<(serde_json::Value, jsonschema::Validator)>> = DashMap::new();

    let mut offset = 0u64;
    let mut count = 0u64;
    let mut current_tx_entries = Vec::new();
    let mut current_tx_id = None;

    storage.stream_log_into(&mut |entry, length| {
        // Condition 1: Check Timestamp
        if let Some(t) = to_time {
            if entry._t > t {
                return ControlFlow::Break(());
            }
        }

        // Condition 2: Check Sequence
        if let Some(s) = to_seq {
            if count >= s {
                return ControlFlow::Break(());
            }
        }

        let pointer = crate::engine::types::RecordPointer {
            offset,
            length,
        };

        match entry.cmd.as_str() {
            "TX_BEGIN" => {
                current_tx_id = Some(entry.key.clone());
                current_tx_entries.clear();
            }
            "TX_COMMIT" => {
                if current_tx_id.as_ref() == Some(&entry.key) {
                    for (e, p) in current_tx_entries.drain(..) {
                        crate::engine::storage::apply_entry(
                            &e,
                            &state,
                            &indexes,
                            #[cfg(feature = "schema")] &schemas,
                            Some(p),
                        );
                    }
                    current_tx_id = None;
                }
            }
            _ => {
                if current_tx_id.is_some() {
                    current_tx_entries.push((entry, pointer));
                } else {
                    crate::engine::storage::apply_entry(
                        &entry,
                        &state,
                        &indexes,
                        #[cfg(feature = "schema")] &schemas,
                        Some(pointer),
                    );
                }
            }
        }

        count += 1;
        offset += (length + 1) as u64;
        ControlFlow::Continue(())
    })?;

    // Convert the recovered state into LogEntries (similar to compact logic)
    let mut entries = Vec::new();
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
                    let bytes = storage.read_at(ptr.offset, ptr.length).unwrap_or_default();
                    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                        LogEntry::new("INSERT".to_string(), col_name.clone(), item_ref.key().clone(), serde_json::Value::Null)
                    })
                }
            };
            entries.push(entry);
        }
    }

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

    Ok(entries)
}
