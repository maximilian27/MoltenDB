// ─── engine/mod.rs ────────────────────────────────────────────────────────────
// This is the root module of the database engine. It defines the `Db` struct —
// the central object that the rest of the application interacts with.
//
// The Db struct is a thin, cloneable handle to the shared database state.
// Cloning a Db is cheap — it just increments reference counts on the Arcs
// inside. All clones share the same underlying data, so any write made through
// one clone is immediately visible through all others. This is how Axum handler
// functions can each receive their own Db clone via State<> extraction while
// all operating on the same in-memory database.
//
// Internal structure:
//   state        — the actual document data: collection → (key → JSON value)
//   storage      — the persistence layer (disk, encrypted, or OPFS)
//   tx           — broadcast channel for real-time WebSocket notifications
//   indexes      — field indexes for fast WHERE queries
//   query_heatmap — tracks query frequency for auto-indexing
//
// The Db struct has two constructors:
//   open()      — native (server) build, opens a disk file
//   open_wasm() — WASM (browser) build, opens an OPFS file
// Both are conditionally compiled with #[cfg(...)] attributes.
// ─────────────────────────────────────────────────────────────────────────────

// Declare the sub-modules of the engine.
mod types;      // LogEntry, DbError
mod indexing;   // index_doc, unindex_doc, track_query, create_index
mod storage;    // StorageBackend trait + concrete implementations
mod operations; // get, get_all, insert_batch, update, delete, etc.

// Re-export DbError so callers can write `engine::DbError` instead of
// `engine::types::DbError`.
pub use types::DbError;
// Re-export the StorageBackend trait so callers can use it without knowing
// the internal module structure.
pub use storage::StorageBackend;

// Conditionally re-export concrete storage types based on compile target.
// On native builds (server), expose the disk and encrypted storage types.
// On WASM builds (browser), expose the OPFS storage type.
#[cfg(not(target_arch = "wasm32"))]
pub use storage::{AsyncDiskStorage, SyncDiskStorage, EncryptedStorage, TieredStorage};

#[cfg(target_arch = "wasm32")]
pub use storage::OpfsStorage;

// DashMap = concurrent hash map. DashSet = concurrent hash set.
use dashmap::{DashMap, DashSet};
use tracing::{info, debug};
// Value = dynamically-typed JSON value.
use serde_json::Value;
// Standard HashMap — used for return values from get operations.
use std::collections::HashMap;
// Arc = thread-safe reference-counted pointer.
// Wrapping fields in Arc allows Db to be cheaply cloned — all clones share
// the same underlying data.
use std::sync::Arc;
// Tokio's broadcast channel: one sender, many receivers.
// Used to push real-time change notifications to WebSocket subscribers.
use tokio::sync::broadcast;

/// The central database handle. Cheap to clone — all clones share the same state.
///
/// This struct is the public API of the engine. All database operations go
/// through methods on this struct, which delegate to the operations module.
#[derive(Clone)]
pub struct Db {
    /// The main document store.
    /// Outer map: collection name (e.g. "users") → inner map.
    /// Inner map: document key (e.g. "u1") → JSON document value.
    /// DashMap allows concurrent reads and writes from multiple threads.
    state: Arc<DashMap<String, DashMap<String, Value>>>,

    /// The storage backend — handles persistence to disk or OPFS.
    /// `pub` so handlers can access it directly if needed (e.g. for compaction).
    /// `Arc<dyn StorageBackend>` = shared pointer to any type implementing the trait.
    pub storage: Arc<dyn StorageBackend>,

    /// Broadcast channel sender for real-time change notifications.
    /// When a document is inserted, updated, or deleted, a JSON event is sent
    /// on this channel. WebSocket handlers subscribe to receive these events.
    /// `pub` so the WebSocket handler in main.rs can call subscribe().
    pub tx: broadcast::Sender<String>,

    /// The index store.
    /// Key format: "collection:field" (e.g. "users:role").
    /// Value: field_value → set of document keys with that value.
    /// e.g. "users:role" → { "admin" → {"u1"}, "user" → {"u2", "u3"} }
    /// `pub` so handlers.rs can check for index existence directly.
    pub indexes: Arc<DashMap<String, DashMap<String, DashSet<String>>>>,

    /// Query frequency counter for auto-indexing.
    /// Key: "collection:field". Value: number of times queried.
    /// When a field reaches 3 queries, an index is auto-created.
    pub query_heatmap: Arc<DashMap<String, u32>>,
}

impl Db {
    /// Open (or create) a database at the given file path.
    /// Only available on native (non-WASM) builds.
    ///
    /// `sync_mode`      — if true, use SyncDiskStorage (flush on every write).
    ///                    if false, use AsyncDiskStorage (flush every 50ms).
    ///                    Ignored when `tiered_mode` is true.
    /// `tiered_mode`    — if true, use TieredStorage (hot + cold two-tier backend).
    ///                    Hot writes go to the active log; cold data is archived and
    ///                    read via mmap on startup. Best for large datasets (100k+ docs).
    ///                    Enable with STORAGE_MODE=tiered environment variable.
    /// `encryption_key` — if Some, wrap the storage in EncryptedStorage.
    ///                    if None, data is stored in plaintext (not recommended).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(path: &str, sync_mode: bool, tiered_mode: bool, encryption_key: Option<&[u8; 32]>) -> Result<Self, DbError> {
        // Create the shared in-memory state containers.
        let state = Arc::new(DashMap::new());
        // Create the broadcast channel with a buffer of 100 messages.
        // If the buffer fills up (no subscribers reading), old messages are dropped.
        let (tx, _rx) = broadcast::channel(100);
        let indexes: Arc<DashMap<String, DashMap<String, DashSet<String>>>> =
            Arc::new(Default::default());
        let query_heatmap = Arc::new(Default::default());

        // Choose the base storage backend based on the configured mode.
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
        let base_storage: Arc<dyn StorageBackend> = if tiered_mode {
            Arc::new(storage::TieredStorage::new(path)?)
        } else if sync_mode {
            Arc::new(storage::SyncDiskStorage::new(path)?)
        } else {
            Arc::new(storage::AsyncDiskStorage::new(path)?)
        };

        // Optionally wrap the base storage in EncryptedStorage.
        // EncryptedStorage is transparent — it encrypts on write and decrypts
        // on read, so the rest of the engine doesn't know encryption is happening.
        let storage: Arc<dyn StorageBackend> = if let Some(key) = encryption_key {
            Arc::new(storage::EncryptedStorage::new(base_storage, key))
        } else {
            base_storage
        };

        // Replay the log (or snapshot + delta) into the in-memory state.
        // After this call, `state` and `indexes` reflect the persisted data.
        storage::stream_into_state(&*storage, &state, &indexes)?;

        Ok(Self {
            state,
            storage,
            tx,
            indexes,
            query_heatmap,
        })
    }

    /// Open (or create) a database in the browser using OPFS.
    /// Only available on WASM builds. Async because OPFS APIs return Promises.
    ///
    /// `db_name` — the filename in the OPFS root directory (e.g. "analytics_db").
    #[cfg(target_arch = "wasm32")]
    pub async fn open_wasm(db_name: &str) -> Result<Self, DbError> {
        let state = Arc::new(DashMap::new());
        let (tx, _rx) = broadcast::channel(100);
        let indexes: Arc<DashMap<String, DashMap<String, DashSet<String>>>> =
            Arc::new(Default::default());
        let query_heatmap = Arc::new(Default::default());

        // Open the OPFS file. This is async because the browser's OPFS API
        // uses Promises which we must await.
        let storage: Arc<dyn StorageBackend> = Arc::new(storage::OpfsStorage::new(db_name).await?);

        // Replay the log into the in-memory state.
        storage::stream_into_state(&*storage, &state, &indexes)?;

        Ok(Self {
            state,
            storage,
            tx,
            indexes,
            query_heatmap,
        })
    }

    /// Create a new broadcast receiver for real-time change notifications.
    /// Each call returns an independent receiver — multiple WebSocket handlers
    /// can each subscribe and receive all events independently.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Retrieve a single document by key. Returns None if not found.
    pub fn get(&self, collection: &str, key: &str) -> Option<Value> {
        operations::get(&self.state, collection, key)
    }

    /// Retrieve all documents in a collection as a HashMap.
    pub fn get_all(&self, collection: &str) -> HashMap<String, Value> {
        operations::get_all(&self.state, collection)
    }

    /// Retrieve a specific set of documents by their keys.
    pub fn get_batch(&self, collection: &str, keys: Vec<String>) -> HashMap<String, Value> {
        operations::get_batch(&self.state, collection, keys)
    }

    /// Insert or overwrite multiple documents in one call.
    /// Each item is a (key, value) pair. Writes are persisted to storage.
    pub fn insert_batch(&self, collection: &str, items: Vec<(String, Value)>) -> Result<(), DbError> {
        operations::insert_batch(
            &self.state,
            &self.indexes,
            &self.storage,
            &self.tx,
            collection,
            items,
        )
    }

    /// Partially update a document — merges `updates` into the existing document.
    /// Returns true if the document was found and updated, false if not found.
    pub fn update(&self, collection: &str, key: &str, updates: Value) -> Result<bool, DbError> {
        operations::update(
            &self.state,
            &self.indexes,
            &self.storage,
            &self.tx,
            collection,
            key,
            updates,
        )
    }

    /// Delete a single document by key.
    pub fn delete(&self, collection: &str, key: &str) -> Result<(), DbError> {
        operations::delete(
            &self.state,
            &self.indexes,
            &self.storage,
            &self.tx,
            collection,
            key,
        )
    }

    /// Delete multiple documents by key in one call.
    pub fn delete_batch(&self, collection: &str, keys: Vec<String>) -> Result<(), DbError> {
        operations::delete_batch(
            &self.state,
            &self.indexes,
            &self.storage,
            &self.tx,
            collection,
            keys,
        )
    }

    /// Drop an entire collection — removes all documents and its indexes.
    pub fn delete_collection(&self, collection: &str) -> Result<(), DbError> {
        operations::delete_collection(
            &self.state,
            &self.indexes,
            &self.storage,
            &self.tx,
            collection,
        )
    }

    /// Track that `field` was queried in `collection` and auto-create an index
    /// if this field has been queried 3 or more times.
    /// Errors are silently ignored — auto-indexing is best-effort.
    pub fn track_query(&self, collection: &str, field: &str) {
        // The `let _ =` discards the Result — a failed auto-index is not fatal.
        let _ = indexing::track_query(
            &self.indexes,
            &self.query_heatmap,
            collection,
            field,
            &self.storage,
            &self.state,
        );
    }

    /// Manually create an index on `collection.field`.
    /// If the index already exists, this is a no-op.
    pub fn create_index(&self, collection: &str, field: &str) -> Result<(), DbError> {
        indexing::create_index(&self.indexes, &self.storage, &self.state, collection, field)
    }

    /// Compact the log file — rewrite it to contain only the current state.
    ///
    /// This removes all dead entries (superseded INSERTs, DELETE tombstones)
    /// and writes a binary snapshot for fast next startup.
    ///
    /// The compacted log contains:
    ///   - One INSERT entry per live document (current value only).
    ///   - One INDEX entry per registered index (index data is rebuilt on replay).
    pub fn compact(&self) -> Result<(), DbError> {
        info!("🔨 Starting Log Compaction...");

        // Build the minimal set of entries representing the current state.
        let mut entries = Vec::new();

        // One INSERT per live document across all collections.
        for col_ref in self.state.iter() {
            let col_name = col_ref.key();
            for item_ref in col_ref.value().iter() {
                entries.push(types::LogEntry {
                    cmd: "INSERT".to_string(),
                    collection: col_name.clone(),
                    key: item_ref.key().clone(),
                    value: item_ref.value().clone(),
                });
            }
        }

        // One INDEX entry per registered index.
        // The index name format is "collection:field" — we split it to get both parts.
        for index_ref in self.indexes.iter() {
            let parts: Vec<&str> = index_ref.key().split(':').collect();
            if parts.len() == 2 {
                entries.push(types::LogEntry {
                    cmd: "INDEX".to_string(),
                    collection: parts[0].to_string(),
                    key: parts[1].to_string(),       // field name
                    value: serde_json::json!(null),
                });
            }
        }

        // Delegate the actual file rewrite (and snapshot write) to the storage backend.
        self.storage.compact(entries)?;

        info!("✅ Log Compaction Finished!");
        Ok(())
    }
}
