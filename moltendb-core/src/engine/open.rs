// ─── engine/open.rs ───────────────────────────────────────────────────────────
// Native (non-WASM) constructor for the Db struct.
// Opens or creates a database at the given file path.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicBool;
#[cfg(not(target_arch = "wasm32"))]
use dashmap::{DashMap, DashSet};
#[cfg(not(target_arch = "wasm32"))]
use tokio::sync::broadcast;

use crate::engine::Db;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::DbError;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::config::DbConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::storage;

impl Db {
    /// Open (or create) a database at the given file path.
    /// Only available on native (non-WASM) builds.
    ///
    /// `sync_mode`      — if true, use SyncDiskStorage (flush on every write).
    ///                    if false, use AsyncDiskStorage (flush every 50ms).
    ///                    Ignored when `tiered_mode` is true.
    /// `encryption_key` — if Some, wrap the storage in EncryptedStorage.
    ///                    if None, data is stored in plaintext (not recommended).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(config: DbConfig) -> Result<Self, DbError> {
        let path = &config.path;
        let sync_mode = config.sync_mode;
        let tiered_mode = config.tiered_mode;
        let hot_threshold = config.hot_threshold;
        let rate_limit_requests = config.rate_limit_requests.unwrap_or(1000);
        let rate_limit_window = config.rate_limit_window.unwrap_or(60);
        let max_body_size = config.max_body_size;
        let max_keys_per_request = config.max_keys_per_request;
        let encryption_key = config.encryption_key;
        let post_backup_script = config.post_backup_script;
        let in_memory = config.in_memory;

        // Create the shared in-memory state containers.
        let state = Arc::new(DashMap::new());
        // Create the broadcast channel with a buffer of 100 messages.
        // If the buffer fills up (no subscribers reading), old messages are dropped.
        let (tx, _rx) = broadcast::channel(1000);
        let indexes: Arc<DashMap<String, DashMap<String, DashSet<String>>>> =
            Arc::new(Default::default());
        let query_heatmap = Arc::new(Default::default());
        #[cfg(feature = "schema")]
        let schemas = Arc::new(DashMap::new());

        // Ensure the parent directory exists (skipped in in-memory mode — no file is created).
        if !in_memory
            && let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }

        // Choose the base storage backend based on the configured mode.
        //
        //   in_memory = true    → InMemoryStorage: all data lives in the DashMap only.
        //                         No disk I/O at all. Data is lost on exit.
        //                         Ideal for ephemeral caches and CI environments.
        //
        //   tiered_mode = true  → TieredStorage: hot log (async writes) + cold log
        //                         (mmap reads). Best for large datasets. The cold log
        //                         accumulates promoted hot data and is paged by the OS.
        //
        //   sync_mode = true    → SyncDiskStorage: every write is flushed to disk
        //                         immediately. Zero data loss, lower throughput.
        //
        //   default             → AsyncDiskStorage: writes buffered in memory, flushed
        //                         every 50ms. Highest throughput, up to 50ms data loss.
        // For AsyncDiskStorage, capture the io_fault flag before wrapping in Arc.
        let mut io_fault_arc: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let base_storage: Arc<dyn crate::engine::storage::StorageBackend> = if in_memory {
            Arc::new(storage::InMemoryStorage)
        } else if tiered_mode {
            Arc::new(storage::TieredStorage::new(path)?)
        } else if sync_mode {
            Arc::new(storage::SyncDiskStorage::new(path)?)
        } else {
            let async_storage = storage::AsyncDiskStorage::new(path)?;
            io_fault_arc = Arc::clone(&async_storage.io_fault);
            Arc::new(async_storage)
        };

        // Optionally wrap the base storage in EncryptedStorage.
        // Encryption is skipped in in-memory mode — there is nothing to encrypt on disk.
        // EncryptedStorage is transparent — it encrypts on write and decrypts
        // on read, so the rest of the engine doesn't know encryption is happening.
        let storage: Arc<dyn crate::engine::storage::StorageBackend> = if !in_memory {
            if let Some(key) = encryption_key {
                Arc::new(storage::EncryptedStorage::new(base_storage, &key))
            } else {
                base_storage
            }
        } else {
            base_storage
        };

        // Replay the log (or snapshot + delta) into the in-memory state.
        // After this call, `state` and `indexes` reflect the persisted data.
        storage::stream_into_state(
            &*storage,
            &state,
            &indexes,
            #[cfg(feature = "schema")] &schemas,
        )?;

        Ok(Self {
            state,
            storage,
            tx,
            indexes,
            query_heatmap,
            hot_threshold,
            rate_limit_requests,
            rate_limit_window,
            max_body_size,
            max_keys_per_request,
            #[cfg(feature = "schema")]
            schemas,
            post_backup_script,
            tiered_mode,
            io_fault: io_fault_arc,
            #[cfg(not(target_arch = "wasm32"))]
            started_at: std::time::Instant::now(),
        })
    }
}
