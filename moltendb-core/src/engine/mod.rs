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
mod config;     // DbConfig struct
#[cfg(feature = "schema")]
mod schema;     // JSON Schema validation
mod operations; // get, get_all, insert_batch, update, delete, etc.

// Re-export LogEntry so it can be used by tests and other crates.
pub use types::{DbError, LogEntry};
// Re-export DbConfig
pub use config::DbConfig;
// Re-export the StorageBackend trait so callers can use it without knowing
// the internal module structure.
pub use storage::{StorageBackend, EncryptedStorage};
#[cfg(not(target_arch = "wasm32"))]
pub use storage::{AsyncDiskStorage, SyncDiskStorage};

// DashMap = concurrent hash map. DashSet = concurrent hash set.
use dashmap::{DashMap, DashSet};
use tracing::{info};
// Value = dynamically-typed JSON value.
use serde_json::Value;
// Standard HashMap — used for return values from get operations.
use std::collections::HashMap;
// Arc = thread-safe reference-counted pointer.
// Wrapping fields in Arc allows Db to be cheaply cloned — all clones share
// the same underlying data.
use std::ops::ControlFlow;
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
    /// Inner map: document key (e.g. "u1") → Hybrid Hot/Cold document state.
    /// DashMap allows concurrent reads and writes from multiple threads.
    state: Arc<DashMap<String, DashMap<String, crate::engine::types::DocumentState>>>,

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

    /// The maximum number of documents per collection to keep in RAM (Hot).
    /// If a collection exceeds this, older documents are paged out to disk (Cold).
    /// Default is 50,000.
    pub hot_threshold: usize,

    /// Max requests per window.
    pub rate_limit_requests: u32,

    /// Window size in seconds.
    pub rate_limit_window: u64,

    /// Maximum request body size in bytes.
    pub max_body_size: usize,

    /// Maximum keys allowed per request.
    pub max_keys_per_request: usize,

    /// Registered JSON schemas per collection.
    /// Key: collection name → Value: (Original JSON, Compiled Validator).
    #[cfg(feature = "schema")]
    pub schemas: Arc<DashMap<String, Arc<(Value, jsonschema::Validator)>>>,

    /// Optional shell command to execute after a successful backup.
    /// Supports the {SNAPSHOT_PATH} placeholder.
    pub post_backup_script: Option<String>,

    /// Whether tiered (hot+cold) storage mode is active.
    pub tiered_mode: bool,

    /// Timestamp of when this Db instance was opened, used for uptime calculation.
    pub started_at: std::time::Instant,
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
    pub fn open(config: DbConfig) -> Result<Self, DbError> {
        let path = &config.path;
        let sync_mode = config.sync_mode;
        let tiered_mode = config.tiered_mode;
        let hot_threshold = config.hot_threshold;
        let rate_limit_requests = config.rate_limit_requests;
        let rate_limit_window = config.rate_limit_window;
        let max_body_size = config.max_body_size;
        let max_keys_per_request = config.max_keys_per_request;
        let encryption_key = config.encryption_key;
        let post_backup_script = config.post_backup_script;

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

        // Ensure the parent directory exists.
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }

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
            Arc::new(storage::EncryptedStorage::new(base_storage, &key))
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
            started_at: std::time::Instant::now(),
        })
    }

    /// Open (or create) a database in the browser using OPFS.
    /// Only available on WASM builds. Async because OPFS APIs return Promises.
    ///
    /// `db_name` — the filename in the OPFS root directory (e.g. "analytics_db").
    #[cfg(target_arch = "wasm32")]
    pub async fn open_wasm(config: DbConfig) -> Result<Self, DbError> {
        let db_name = &config.path;
        let hot_threshold = config.hot_threshold;
        let rate_limit_requests = config.rate_limit_requests;
        let rate_limit_window = config.rate_limit_window;
        let max_body_size = config.max_body_size;
        let max_keys_per_request = config.max_keys_per_request;
        let encryption_key = config.encryption_key;
        let sync_mode = config.sync_mode;
        let post_backup_script = config.post_backup_script;

        let state = Arc::new(DashMap::new());
        let (tx, _rx) = broadcast::channel(1000);
        let indexes: Arc<DashMap<String, DashMap<String, DashSet<String>>>> =
            Arc::new(Default::default());
        let query_heatmap = Arc::new(Default::default());
        #[cfg(feature = "schema")]
        let schemas = Arc::new(DashMap::new());

        // Open the OPFS file. This is async because the browser's OPFS API
        // uses Promises which we must await.
        let mut storage: Arc<dyn StorageBackend> =
            Arc::new(storage::OpfsStorage::new(db_name, sync_mode).await?);

        // Apply encryption wrapper if a key is provided.
        if let Some(key) = encryption_key {
            storage = Arc::new(storage::EncryptedStorage::new(storage, &key));
        }

        // Replay the log into the in-memory state.
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
            tiered_mode: config.tiered_mode,
            started_at: std::time::Instant::now(),
        })
    }

    /// Returns the total number of hot (in-memory) keys across all collections.
    pub fn hot_keys_count(&self) -> usize {
        self.state.iter().map(|c| c.value().len()).sum()
    }

    /// Create a new broadcast receiver for real-time change notifications.
    /// Each call returns an independent receiver — multiple WebSocket handlers
    /// can each subscribe and receive all events independently.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Retrieve a single document by key. Returns None if not found.
    pub fn get(&self, collection: &str, key: &str) -> Option<Value> {
        operations::get(&self.state, &self.storage, collection, key)
    }

    /// Retrieve all documents in a collection as a HashMap.
    pub fn get_all(&self, collection: &str) -> HashMap<String, Value> {
        operations::get_all(&self.state, &self.storage, collection)
    }

    /// Retrieve a specific set of documents by their keys.
    pub fn get_batch(&self, collection: &str, keys: Vec<String>) -> HashMap<String, Value> {
        operations::get_batch(&self.state, &self.storage, collection, keys)
    }

    /// Insert or overwrite multiple documents in one call.
    /// Each item is a (key, value) pair. Writes are persisted to storage.
    pub fn insert_batch(&self, collection: &str, items: Vec<(String, Value)>) -> Result<(), DbError> {
        operations::insert_batch(
            &self.state,
            &self.indexes,
            &self.storage,
            &self.tx,
            #[cfg(feature = "schema")] &self.schemas,
            collection,
            items,
        )?;

        // Auto-evict if the collection exceeds the threshold.
        let _ = self.evict_collection(collection, self.hot_threshold);
        Ok(())
    }

    /// Partially update a document — merges `updates` into the existing document.
    /// Returns true if the document was found and updated, false if not found.
    pub fn update(&self, collection: &str, key: &str, updates: Value) -> Result<bool, DbError> {
        let updated = operations::update(
            &self.state,
            &self.indexes,
            &self.storage,
            &self.tx,
            #[cfg(feature = "schema")] &self.schemas,
            collection,
            key,
            updates,
        )?;

        if updated {
            // Auto-evict if the collection exceeds the threshold.
            let _ = self.evict_collection(collection, self.hot_threshold);
        }
        Ok(updated)
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

    /// Register a JSON schema for a collection.
    /// All subsequent writes to this collection must conform to this schema.
    #[cfg(feature = "schema")]
    pub fn set_schema(&self, collection: &str, schema: Value) -> Result<(), DbError> {
        schema::set_schema(
            &self.schemas,
            &self.storage,
            &self.tx,
            collection,
            schema
        )
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
                // To compact, we need the full Value. If it's Cold, we fetch it from storage.
                let entry = match item_ref.value() {
                    crate::engine::types::DocumentState::Hot(v) => {
                        types::LogEntry::new(
                            "INSERT".to_string(),
                            col_name.clone(),
                            item_ref.key().clone(),
                            v.clone(),
                        )
                    }
                    crate::engine::types::DocumentState::Cold(ptr) => {
                        let bytes = self.storage.read_at(ptr.offset, ptr.length)?;
                        serde_json::from_slice(&bytes)?
                    }
                };
                entries.push(entry);
            }
        }

        // One SCHEMA entry per collection.
        #[cfg(feature = "schema")]
        for schema_ref in self.schemas.iter() {
            let col_name = schema_ref.key();
            let (schema_json, _) = &**schema_ref.value();
            entries.push(types::LogEntry::new(
                "SCHEMA".to_string(),
                col_name.clone(),
                "".to_string(),
                schema_json.clone(),
            ));
        }

        // One INDEX entry per registered index.
        // The index name format is "collection:field" — we split it to get both parts.
        for index_ref in self.indexes.iter() {
            let parts: Vec<&str> = index_ref.key().split(':').collect();
            if parts.len() == 2 {
                entries.push(types::LogEntry::new(
                    "INDEX".to_string(),
                    parts[0].to_string(),
                    parts[1].to_string(),       // field name
                    serde_json::json!(null),
                ));
            }
        }

        // Delegate the actual file rewrite (and snapshot write) to the storage backend.
        self.storage.compact_with_hook(entries.clone(), self.post_backup_script.clone())?;

        // After compaction the log is rewritten and all old RecordPointers are invalid.
        // Promote every Cold entry in the in-memory state to Hot so subsequent reads
        // don't try to seek to stale byte offsets in the now-truncated log file.
        for entry in &entries {
            if entry.cmd == "INSERT" {
                if let Some(col) = self.state.get(&entry.collection) {
                    if let Some(mut doc) = col.get_mut(&entry.key) {
                        if matches!(*doc, crate::engine::types::DocumentState::Cold(_)) {
                            *doc = crate::engine::types::DocumentState::Hot(entry.value.clone());
                        }
                    }
                }
            }
        }

        info!("✅ Log Compaction Finished!");
        Ok(())
    }

    /// Evict documents from RAM to disk for a collection if it exceeds the threshold.
    ///
    /// This converts `Hot(Value)` entries into `Cold(RecordPointer)` entries.
    /// In this v1, it re-scans the log to find the exact byte offsets for the documents.
    pub fn evict_collection(&self, collection: &str, limit: usize) -> Result<usize, DbError> {
        let col_len = if let Some(col) = self.state.get(collection) {
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
        self.storage.stream_log_into(&mut |entry, length| {
            if entry.collection == collection {
                if evicted_count < to_evict {
                    if let Some(col) = self.state.get(collection) {
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

    /// Recover the database state to a specific point in time or sequence number.
    /// Returns the recovered state as a Vec of LogEntries that can be written to a snapshot.
    ///
    /// This is a utility function used by the CLI for PITR.
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
}
