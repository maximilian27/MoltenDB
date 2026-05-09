// ─── engine/open_wasm.rs ──────────────────────────────────────────────────────
// WASM constructor for the Db struct.
// Opens or creates a database in the browser using OPFS.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use std::sync::atomic::AtomicBool;
#[cfg(target_arch = "wasm32")]
use dashmap::DashMap;
#[cfg(target_arch = "wasm32")]
use tokio::sync::broadcast;

use crate::engine::Db;
#[cfg(target_arch = "wasm32")]
use crate::engine::DbError;
#[cfg(target_arch = "wasm32")]
use crate::engine::config::DbConfig;
#[cfg(target_arch = "wasm32")]
use crate::engine::storage;

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
        let post_backup_script = config.post_backup_script;

        let state = Arc::new(DashMap::new());
        let (tx, _rx) = broadcast::channel(1000);
        #[cfg(feature = "schema")]
        let schemas = Arc::new(DashMap::new());

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
                Arc::new(storage::EncryptedStorage::new(base, &key)) as Arc<dyn crate::engine::storage::StorageBackend>
            } else {
                base
            };

            // Replay the log into the in-memory state.
            storage::stream_into_state(
                &*wrapped,
                &state,
                #[cfg(feature = "schema")] &schemas,
            )?;

            wrapped
        };

        Ok(Self {
            state,
            storage,
            tx,
            rate_limit_requests,
            rate_limit_window,
            max_body_size,
            max_keys_per_request,
            #[cfg(feature = "schema")]
            schemas,
            post_backup_script,
            io_fault: Arc::new(AtomicBool::new(false)),
        })
    }
}
