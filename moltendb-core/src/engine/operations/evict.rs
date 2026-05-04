// ─── operations/evict.rs ──────────────────────────────────────────────────────
// Evicts documents from RAM to disk for a collection if it exceeds the threshold.
// ─────────────────────────────────────────────────────────────────────────────

use std::ops::ControlFlow;
use dashmap::DashMap;
use crate::engine::types::DbError;
use crate::engine::storage::StorageBackend;

pub fn evict_collection(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &dyn StorageBackend,
    collection: &str,
    limit: usize,
) -> Result<usize, DbError> {
    let col_len = if let Some(col) = state.get(collection) {
        col.len()
    } else {
        return Err(DbError::CollectionNotFound);
    };

    if col_len <= limit {
        return Ok(0);
    }

    let mut evicted_count = 0;
    let mut offset = 0u64;
    let to_evict = col_len - limit;

    // To evict properly, we need the pointers. Since we don't store them for
    // Hot documents, we re-scan the log to find them.
    storage.stream_log_into(&mut |entry, length| {
        if entry.collection == collection {
            if evicted_count < to_evict {
                if let Some(col) = state.get(collection) {
                    if let Some(mut doc_state) = col.get_mut(&entry.key) {
                        if let crate::engine::types::DocumentState::Hot(_) = *doc_state {
                            *doc_state = crate::engine::types::DocumentState::Cold(crate::engine::types::RecordPointer {
                                offset,
                                length,
                            });
                            evicted_count += 1;
                        }
                    }
                }
            }
        }
        offset += (length + 1) as u64;
        ControlFlow::Continue(())
    })?;

    Ok(evicted_count)
}
