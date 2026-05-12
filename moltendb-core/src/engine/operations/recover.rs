#[cfg(not(target_arch = "wasm32"))]
use std::ops::ControlFlow;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use dashmap::DashMap;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::Value;
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
    let state: DashMap<Arc<str>, DashMap<String, Box<[u8]>>> = DashMap::new();
    #[cfg(feature = "schema")]
    let schemas: DashMap<String, Arc<(serde_json::Value, jsonschema::Validator)>> = DashMap::new();
    let mut count = 0u64;
    let mut current_tx_entries: Vec<LogEntry> = Vec::new();
    let mut current_tx_id = None;

    storage.stream_log_into(&mut |entry, _length| {
        if let Some(t) = to_time && entry._t > t {
            return ControlFlow::Break(());
        }
        if let Some(s) = to_seq && count >= s {
            return ControlFlow::Break(());
        }
        match entry.cmd.as_str() {
            "TX_BEGIN" => {
                current_tx_id = Some(entry.key.clone());
                current_tx_entries.clear();
            }
            "TX_COMMIT" => {
                if current_tx_id.as_ref() == Some(&entry.key) {
                    for e in current_tx_entries.drain(..) {
                        crate::engine::storage::apply_entry(
                            &e,
                            &state,
                            #[cfg(feature = "schema")] &schemas,
                        );
                    }
                    current_tx_id = None;
                }
            }
            _ => {
                if current_tx_id.is_some() {
                    current_tx_entries.push(entry);
                } else {
                    crate::engine::storage::apply_entry(
                        &entry,
                        &state,
                        #[cfg(feature = "schema")] &schemas,
                    );
                }
            }
        }
        count += 1;
        ControlFlow::Continue(())
    })?;

    // Convert the recovered state into LogEntries.
    let mut entries = Vec::new();
    for col_ref in state.iter() {
        let col_name = col_ref.key();
        for item_ref in col_ref.value().iter() {
            if let Ok(value) = rmp_serde::from_slice::<Value>(item_ref.value()) {
                entries.push(LogEntry::new(
                    "INSERT".to_string(),
                    col_name.to_string(),
                    item_ref.key().clone(),
                    value,
                ));
            }
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
    Ok(entries)
}
