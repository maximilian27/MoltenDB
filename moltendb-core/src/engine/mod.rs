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
//
// The Db struct has two constructors:
//   open()      — native (server) build, opens a disk file
//   open_wasm() — WASM (browser) build, opens an OPFS file
// Both are conditionally compiled with #[cfg(...)] attributes.
// ─────────────────────────────────────────────────────────────────────────────

// Declare the sub-modules of the engine.
mod types;      // LogEntry, DbError
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

use dashmap::DashMap;
use serde_json::Value;
#[allow(unused_imports)]
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::broadcast;

/// The central database handle. Cheap to clone — all clones share the same state.
///
/// This struct is the public API of the engine. All database operations go
/// through methods on this struct, which delegate to the operations module.
#[derive(Clone)]
pub struct Db {
    /// The main document store.
    /// Outer map: collection name (e.g. "users") → inner map.
    /// Inner map: document key (e.g. "u1") → document value (always in RAM).
    /// DashMap allows concurrent reads and writes from multiple threads.
    state: Arc<DashMap<Arc<str>, DashMap<String, Box<[u8]>>>>,  // documents stored as MsgPack bytes

    /// The storage backend — handles persistence to disk or OPFS.
    /// `pub` so handlers can access it directly if needed (e.g. for compaction).
    /// `Arc<dyn StorageBackend>` = shared pointer to any type implementing the trait.
    pub storage: Arc<dyn StorageBackend>,

    /// Broadcast channel sender for real-time change notifications.
    /// When a document is inserted, updated, or deleted, a JSON event is sent
    /// on this channel. WebSocket handlers subscribe to receive these events.
    /// `pub` so the WebSocket handler in main.rs can call subscribe().
    pub tx: broadcast::Sender<String>,


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

    /// Circuit-breaker flag shared with `AsyncDiskStorage`.
    /// When the background writer encounters a fatal I/O error it sets this to
    /// `true`. All subsequent write operations return `DbError::StorageFault`
    /// immediately, preventing silent data loss.
    pub io_fault: Arc<AtomicBool>,

    /// Timestamp of when this Db instance was opened, used for uptime calculation.
    #[cfg(not(target_arch = "wasm32"))]
    pub started_at: std::time::Instant,
}

impl Db {
    /// Returns the total number of hot (in-memory) keys across all collections.
    pub fn hot_keys_count(&self) -> usize {
        self.state.iter().map(|c: dashmap::mapref::multiple::RefMulti<_, _>| c.value().len()).sum::<usize>()
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

    /// Lazily scan a collection, returning only documents that match `predicate`.
    ///
    /// Avoids the full O(n) clone that `get_all` does — only matching documents
    /// are cloned. `offset` and `limit` are applied during iteration so the
    /// scan can stop early. Used for WHERE queries on large collections when
    /// no index applies.
    pub fn get_filtered(
        &self,
        collection: &str,
        predicate: impl Fn(&Value) -> bool + Sync,
        offset: usize,
        limit: Option<usize>,
    ) -> HashMap<String, Value> {
        operations::get_filtered(&self.state, &self.storage, collection, predicate, offset, limit)
    }

    /// Lazily scan a collection and return the top-`cap` documents according
    /// to a comparator, applying an optional predicate (e.g. WHERE) along the
    /// way.
    ///
    /// Documents flow directly from the DashMap into a bounded max-heap of
    /// capacity `cap` — peak memory is `O(cap)` extra instead of `O(matching)`,
    /// even for collections of millions of documents. The result is already
    /// sorted best-first (per the comparator); the caller still applies
    /// `offset` and `count` for pagination.
    pub fn scan_top_n(
        &self,
        collection: &str,
        predicate: impl Fn(&Value) -> bool + Sync,
        cmp: impl Fn(&Value, &Value) -> std::cmp::Ordering + Send + Sync,
        cap: usize,
    ) -> Vec<(String, Value)> {
        operations::scan_top_n(&self.state, &self.storage, collection, predicate, cmp, cap)
    }

    /// Insert or overwrite multiple documents in one call.
    /// Each item is a (key, value) pair. Writes are persisted to storage.
    pub fn insert(&self, collection: &str, items: Vec<(String, Value)>) -> Result<(), DbError> {
        if self.io_fault.load(Ordering::Relaxed) {
            return Err(DbError::StorageFault(
                "Background disk I/O failed. System is in read-only mode.".into(),
            ));
        }
        operations::insert(operations::InsertParams {
            state: &self.state,
            storage: &self.storage,
            tx: &self.tx,
            #[cfg(feature = "schema")] schemas: &self.schemas,
            collection,
            items,
        })?;
        Ok(())
    }

    /// Partially update a document — merges `updates` into the existing document.
    /// Returns true if the document was found and updated, false if not found.
    pub fn update(&self, collection: &str, key: &str, updates: Value) -> Result<bool, DbError> {
        if self.io_fault.load(Ordering::Relaxed) {
            return Err(DbError::StorageFault(
                "Background disk I/O failed. System is in read-only mode.".into(),
            ));
        }
        let updated = operations::update(operations::UpdateParams {
            state: &self.state,
            storage: &self.storage,
            tx: &self.tx,
            #[cfg(feature = "schema")] schemas: &self.schemas,
            collection,
            key,
            updates,
        })?;

        Ok(updated)
    }

    /// Scan a collection with a predicate and delete all matching documents.
    /// Mirrors `get_filtered` on the read side. Returns the number of documents deleted.
    pub fn delete_filtered(
        &self,
        collection: &str,
        predicate: impl Fn(&Value) -> bool + Sync,
    ) -> Result<usize, DbError> {
        if self.io_fault.load(Ordering::Relaxed) {
            return Err(DbError::StorageFault(
                "Background disk I/O failed. System is in read-only mode.".into(),
            ));
        }
        operations::delete_filtered(&self.state, &self.storage, &self.tx, collection, predicate)
    }

    /// Delete one or more documents by key. Pass a single key to delete one document.
    pub fn delete(&self, collection: &str, keys: Vec<String>) -> Result<(), DbError> {
        if self.io_fault.load(Ordering::Relaxed) {
            return Err(DbError::StorageFault(
                "Background disk I/O failed. System is in read-only mode.".into(),
            ));
        }
        operations::delete(
            &self.state,
            &self.storage,
            &self.tx,
            collection,
            keys,
        )
    }

    /// Drop an entire collection — removes all documents.
    pub fn delete_collection(&self, collection: &str) -> Result<(), DbError> {
        operations::delete_collection(
            &self.state,
            &self.storage,
            &self.tx,
            collection,
        )
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
    
    /// Wipe all in-memory state.
    /// Used by the WASM layer when a browser tab unloads in in-memory mode,
    /// so that any tab refresh clears the shared RAM store for all tabs.
    pub fn clear_all(&self) {
        self.state.clear();
        #[cfg(feature = "schema")]
        self.schemas.clear();
    }

    /// Compact the log file — rewrite it to contain only the current state.
    ///
    /// This removes all dead entries (superseded INSERTs, DELETE tombstones)
    /// and writes a binary snapshot for fast next startup.
    pub fn compact(&self) -> Result<(), DbError> {
        operations::compact(
            &self.state,
            #[cfg(feature = "schema")] &self.schemas,
            &*self.storage,
            self.post_backup_script.clone(),
        )?;
        Ok(())
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
