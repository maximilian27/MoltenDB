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
mod operations; // get, get_all, insert, update, delete, etc.
mod open;       // Db::open() — native constructor
mod open_wasm;  // Db::open_wasm() — WASM constructor

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
    #[cfg(not(target_arch = "wasm32"))]
    pub started_at: std::time::Instant,
}

impl Db {
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

    /// Retrieve documents by their keys. Returns a HashMap of found key→value pairs.
    /// Missing keys are silently skipped. Pass a single key to retrieve one document.
    pub fn get(&self, collection: &str, keys: Vec<String>) -> HashMap<String, Value> {
        operations::get(&self.state, &self.storage, collection, keys)
    }

    /// Retrieve all documents in a collection as a HashMap.
    pub fn get_all(&self, collection: &str) -> HashMap<String, Value> {
        operations::get_all(&self.state, &self.storage, collection)
    }

    /// Insert or overwrite multiple documents in one call.
    /// Each item is a (key, value) pair. Writes are persisted to storage.
    pub fn insert(&self, collection: &str, items: Vec<(String, Value)>) -> Result<(), DbError> {
        operations::insert(operations::InsertParams {
            state: &self.state,
            indexes: &self.indexes,
            storage: &self.storage,
            tx: &self.tx,
            #[cfg(feature = "schema")] schemas: &self.schemas,
            collection,
            items,
        })?;

        // Auto-evict if the collection exceeds the threshold.
        let _ = self.evict_collection(collection, self.hot_threshold);
        Ok(())
    }

    /// Partially update a document — merges `updates` into the existing document.
    /// Returns true if the document was found and updated, false if not found.
    pub fn update(&self, collection: &str, key: &str, updates: Value) -> Result<bool, DbError> {
        let updated = operations::update(operations::UpdateParams {
            state: &self.state,
            indexes: &self.indexes,
            storage: &self.storage,
            tx: &self.tx,
            #[cfg(feature = "schema")] schemas: &self.schemas,
            collection,
            key,
            updates,
        })?;

        if updated {
            // Auto-evict if the collection exceeds the threshold.
            let _ = self.evict_collection(collection, self.hot_threshold);
        }
        Ok(updated)
    }

    /// Delete one or more documents by key. Pass a single key to delete one document.
    pub fn delete(&self, collection: &str, keys: Vec<String>) -> Result<(), DbError> {
        operations::delete(
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
    
    /// Wipe all in-memory state — documents, indexes, and query heatmap.
    /// Used by the WASM layer when a browser tab unloads in in-memory mode,
    /// so that any tab refresh clears the shared RAM store for all tabs.
    pub fn clear_all(&self) {
        self.state.clear();
        self.indexes.clear();
        self.query_heatmap.clear();
        #[cfg(feature = "schema")]
        self.schemas.clear();
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
        let entries = operations::compact(
            &self.state,
            #[cfg(feature = "schema")] &self.schemas,
            &self.indexes,
            &*self.storage,
            self.post_backup_script.clone(),
        )?;

        // After compaction the log is rewritten and all old RecordPointers are invalid.
        // Promote every Cold entry in the in-memory state to Hot so subsequent reads
        // don't try to seek to stale byte offsets in the now-truncated log file.
        for entry in &entries {
            if entry.cmd == "INSERT"
                && let Some(col) = self.state.get(&entry.collection)
                    && let Some(mut doc) = col.get_mut(&entry.key)
                        && matches!(*doc, crate::engine::types::DocumentState::Cold(_)) {
                            *doc = crate::engine::types::DocumentState::Hot(entry.value.clone());
                        }
        }

        Ok(())
    }

    /// Evict documents from RAM to disk for a collection if it exceeds the threshold.
    ///
    /// This converts `Hot(Value)` entries into `Cold(RecordPointer)` entries.
    /// In this v1, it re-scans the log to find the exact byte offsets for the documents.
    pub fn evict_collection(&self, collection: &str, limit: usize) -> Result<usize, DbError> {
        operations::evict_collection(&self.state, &*self.storage, collection, limit)
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
        operations::recover_to(storage, to_time, to_seq)
    }
}
