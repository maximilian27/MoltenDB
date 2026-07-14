// ─── engine/open_wasm.rs ──────────────────────────────────────────────────────
// WASM constructor for the Db struct.
// Opens or creates a database in the browser using OPFS.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
use dashmap::DashMap;
#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;
#[cfg(target_arch = "wasm32")]
use std::sync::atomic::AtomicBool;
#[cfg(target_arch = "wasm32")]
use std::sync::{Arc, RwLock};
#[cfg(target_arch = "wasm32")]
use tokio::sync::broadcast;

#[cfg(target_arch = "wasm32")]
use crate::engine::config::DbConfig;
#[cfg(target_arch = "wasm32")]
use crate::engine::storage;
use crate::engine::Db;
#[cfg(target_arch = "wasm32")]
use crate::engine::DbError;

impl Db {
    /// Open (or create) a database in the browser using OPFS.
    /// Only available on WASM builds. Async because OPFS APIs return Promises.
    ///
    /// `db_name` — the filename in the OPFS root directory (e.g. "analytics_db").
    #[cfg(target_arch = "wasm32")]
    pub async fn open_wasm(config: DbConfig) -> Result<Self, DbError> {
        let db_name = &config.path;
        let rate_limit_requests = config.rate_limit_requests.unwrap_or(1000);
        let rate_limit_window = config.rate_limit_window.unwrap_or(60);
        let max_body_size = config.max_body_size;
        let max_keys_per_request = config.max_keys_per_request;
        let encryption_key = config.encryption_key;
        let sync_mode = config.sync_mode;

        let state = Arc::new(DashMap::new());
        let (tx, _rx) = broadcast::channel(1000);
        #[cfg(feature = "schema")]
        let schemas = Arc::new(DashMap::new());
        let ttl_defaults = Arc::new(DashMap::new());
        let ttl_expiry = Arc::new(DashMap::new());
        let seq_counters = Arc::new(DashMap::new());
        let seq_index = Arc::new(DashMap::new());
        let max_sizes = Arc::new(DashMap::new());

        // Choose storage backend: pure RAM (no OPFS) or OPFS file.
        // When in_memory = true, OpfsStorage is never opened — no file is created
        // and no log is replayed. All data lives only in the DashMap.
        let storage: Arc<dyn crate::engine::storage::StorageBackend> = if config.in_memory {
            Arc::new(storage::InMemoryStorage)
        } else {
            // Open the OPFS file. This is async because the browser's OPFS API
            // uses Promises which we must await.
            let base: Arc<dyn crate::engine::storage::StorageBackend> =
                Arc::new(storage::OpfsStorage::new(db_name, sync_mode).await?);

            // Apply encryption wrapper if a key is provided.
            let wrapped = if let Some(key) = encryption_key {
                Arc::new(storage::EncryptedStorage::new(base, &key))
                    as Arc<dyn crate::engine::storage::StorageBackend>
            } else {
                base
            };

            // Replay the log into the in-memory state.
            storage::stream_into_state(
                &*wrapped,
                &state,
                #[cfg(feature = "schema")]
                &schemas,
                &ttl_expiry,
            )?;

            // Build seq_index from replayed state.
            // Also seed seq_counters to (max_seq + 1) so new inserts after a
            // page refresh don't receive seq values that precede existing documents.
            for col_ref in state.iter() {
                let col_name = col_ref.key().clone();
                let col_map = col_ref.value();
                let mut btree: BTreeMap<u64, String> = BTreeMap::new();
                let mut max_seq: u64 = 0;
                for entry in col_map.iter() {
                    let seq =
                        crate::common::system_field_tokens::read_msgpack_seq_token(entry.value());
                    btree.insert(seq, entry.key().clone());
                    if seq > max_seq {
                        max_seq = seq;
                    }
                }
                seq_index.insert(col_name.clone(), Arc::new(RwLock::new(btree)));
                // Start the counter just above the highest observed seq so that
                // the next insert always appends rather than overwriting an old slot.
                seq_counters.insert(
                    col_name.to_string(),
                    std::sync::atomic::AtomicU64::new(max_seq + 1),
                );
            }

            wrapped
        };

        let db = Self {
            state,
            storage,
            tx,
            rate_limit_requests,
            rate_limit_window,
            max_body_size,
            max_keys_per_request,
            #[cfg(feature = "schema")]
            schemas,
            ttl_defaults,
            ttl_expiry,
            seq_counters,
            seq_index,
            max_sizes,
            io_fault: Arc::new(AtomicBool::new(false)),
        };

        // Startup TTL sweep: drop any collections that already expired while the
        // tab was closed. This mirrors the server's ttl_sweep.rs startup logic.
        // Without this, replayed-from-OPFS expired collections stay in memory
        // until the next GET request triggers the per-request expiry check.
        let now = crate::engine::ttl::now_ms();
        for (col, expires_at) in db.all_ttl_expiries() {
            if expires_at <= now {
                let _ = db.delete_collection(&col);
            }
        }

        Ok(db)
    }
}
