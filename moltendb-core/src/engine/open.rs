// ─── engine/open.rs ───────────────────────────────────────────────────────────
// Native (non-WASM) constructor for the Db struct.
// Opens or creates a database at the given file path.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
use dashmap::DashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicBool;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, RwLock};
#[cfg(not(target_arch = "wasm32"))]
use tokio::sync::broadcast;

#[cfg(not(target_arch = "wasm32"))]
use crate::engine::config::DbConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::storage;
use crate::engine::Db;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::DbError;

impl Db {
    /// Open (or create) a database at the given file path.
    /// Only available on native (non-WASM) builds.
    ///
    /// `sync_mode`      — if true, use SyncDiskStorage (flush on every write).
    ///                    if false, use AsyncDiskStorage (flush every 50ms).
    /// `encryption_key` — if Some, wrap the storage in EncryptedStorage.
    ///                    if None, data is stored in plaintext (not recommended).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(config: DbConfig) -> Result<Self, DbError> {
        let path = &config.path;
        let sync_mode = config.sync_mode;
        let rate_limit_requests = config.rate_limit_requests.unwrap_or(1000);
        let rate_limit_window = config.rate_limit_window.unwrap_or(60);
        let max_body_size = config.max_body_size;
        let max_keys_per_request = config.max_keys_per_request;
        let encryption_key = config.encryption_key;
        let in_memory = config.in_memory;

        // Create the shared in-memory state containers.
        let state = Arc::new(DashMap::new());
        // Create the broadcast channel with a buffer of 100 messages.
        // If the buffer fills up (no subscribers reading), old messages are dropped.
        let (tx, _rx) = broadcast::channel(1000);
        #[cfg(feature = "schema")]
        let schemas = Arc::new(DashMap::new());
        let ttl_defaults = Arc::new(DashMap::new());
        let ttl_expiry = Arc::new(DashMap::new());
        let seq_counters = Arc::new(DashMap::new());
        let seq_index = Arc::new(DashMap::new());
        let max_sizes = Arc::new(DashMap::new());

        // Ensure the parent directory exists (skipped in in-memory mode — no file is created).
        if !in_memory && let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Choose the base storage backend based on the configured mode.
        //
        //   in_memory = true → InMemoryStorage: no disk I/O, data lost on exit.
        //   sync_mode = true → SyncDiskStorage: flush on every write, zero data loss.
        //   default          → AsyncDiskStorage: flush every 50ms, highest throughput.
        let mut io_fault_arc: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let base_storage: Arc<dyn crate::engine::storage::StorageBackend> = if in_memory {
            Arc::new(storage::InMemoryStorage)
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
        storage::stream_into_state(
            &*storage,
            &state,
            #[cfg(feature = "schema")]
            &schemas,
            &ttl_expiry,
        )?;

        // Build the seq_index from the replayed state so ordered queries work
        // immediately after startup without waiting for the first insert.
        for col_ref in state.iter() {
            let col_name = col_ref.key().clone();
            let col_map = col_ref.value();
            let mut btree: BTreeMap<u64, String> = BTreeMap::new();
            for entry in col_map.iter() {
                let seq = crate::common::system_field_tokens::read_msgpack_seq_token(entry.value());
                btree.insert(seq, entry.key().clone());
            }
            seq_index.insert(col_name, Arc::new(RwLock::new(btree)));
        }

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
            ttl_defaults,
            ttl_expiry,
            seq_counters,
            seq_index,
            max_sizes,
            io_fault: io_fault_arc,
            #[cfg(not(target_arch = "wasm32"))]
            started_at: std::time::Instant::now(),
        })
    }
}
