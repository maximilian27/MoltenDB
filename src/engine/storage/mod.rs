// ─── storage/mod.rs ──────────────────────────────────────────────────────────
// This is the root module for all storage backends. It does three things:
//
//   1. Declares and conditionally exposes the concrete backend modules
//      (disk, encrypted, wasm) based on the compile target.
//
//   2. Defines the StorageBackend trait — the single interface that the rest
//      of the engine uses to read/write data. Any type that implements this
//      trait can be used as a storage backend, whether it writes to a disk
//      file, an encrypted file, or a browser OPFS file.
//
//   3. Provides the startup replay functions (stream_into_state, apply_entry,
//      replay_log_entries) that rebuild the in-memory database state from the
//      persistent log on server/worker startup.
//
// The StorageBackend trait is the key abstraction that makes MoltenDB's
// "same engine, different storage" design possible. The engine (mod.rs,
// operations.rs, handlers.rs) never imports a concrete storage type — it
// only ever holds an Arc<dyn StorageBackend>. This means you can swap the
// storage backend without changing any engine code.
// ─────────────────────────────────────────────────────────────────────────────

// ── Conditional module declarations ──────────────────────────────────────────
// These cfg attributes mean "only compile this when NOT targeting wasm32".
// On native (server) builds we get disk.rs and encrypted.rs.
// On WASM (browser) builds we get wasm.rs.
// This prevents browser-incompatible code (file I/O, Tokio tasks) from being
// compiled into the WASM binary.

#[cfg(not(target_arch = "wasm32"))]
mod disk;
#[cfg(not(target_arch = "wasm32"))]
mod encrypted;
// tiered.rs provides MmapLogReader (memory-mapped cold log reads) and
// TieredStorage (hot + cold two-tier backend for large-scale deployments).
#[cfg(not(target_arch = "wasm32"))]
mod tiered;
// Re-export the concrete types so callers can write `storage::AsyncDiskStorage`
// instead of `storage::disk::AsyncDiskStorage`.
#[cfg(not(target_arch = "wasm32"))]
pub use disk::{AsyncDiskStorage, SyncDiskStorage};
#[cfg(not(target_arch = "wasm32"))]
pub use encrypted::EncryptedStorage;
// Re-export TieredStorage so engine/mod.rs and main.rs can use it directly.
#[cfg(not(target_arch = "wasm32"))]
pub use tiered::TieredStorage;

// On WASM builds, expose the browser-side OPFS storage.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::OpfsStorage;

// ── Shared imports ────────────────────────────────────────────────────────────
// These are used by both the trait definition and the replay functions below.
use crate::engine::types::{DbError, LogEntry};
// DashMap is a concurrent hash map — like HashMap but safe to read/write from
// multiple threads simultaneously without a global lock.
// DashSet is the set equivalent.
use dashmap::{DashMap, DashSet};
// serde_json::Value is a dynamically-typed JSON value (can be object, array,
// string, number, bool, or null). All document data is stored as Value.
use serde_json::Value;

// ─── StorageBackend trait ─────────────────────────────────────────────────────
//
// This is the core abstraction of the storage layer. Any type that implements
// these three methods can serve as a MoltenDB storage backend.
//
// The trait requires Send + Sync because the backend is stored inside an
// Arc<dyn StorageBackend> and shared across multiple Tokio tasks/threads.
//   • Send  = the type can be moved to another thread
//   • Sync  = the type can be referenced from multiple threads simultaneously
// ─────────────────────────────────────────────────────────────────────────────

/// The core storage abstraction. Implement this trait to add a new storage backend.
///
/// All three methods operate on `LogEntry` — the atomic unit of data in MoltenDB.
/// The engine never writes raw bytes; it always goes through this interface.
pub trait StorageBackend: Send + Sync {
    /// Append a single log entry to the persistent store.
    ///
    /// This is called on every insert, update, delete, and index creation.
    /// Implementations may buffer writes (async) or flush immediately (sync).
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError>;

    /// Read all log entries from persistent storage into a Vec.
    ///
    /// Called on startup to rebuild the in-memory state, and by EncryptedStorage
    /// which must decrypt entries before they can be streamed into state.
    /// For large databases, prefer `stream_log_into` which avoids holding the
    /// full log in RAM.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError>;

    /// Compact the log by writing only the current state (removing dead entries).
    ///
    /// `entries` is the complete current state of the database — every live
    /// document as a single INSERT entry. The implementation should atomically
    /// replace the existing log with this minimal set.
    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError>;

    /// Return the current size of the persistent log file in bytes.
    ///
    /// Used by the WASM worker to implement size-based auto-compaction — the JS
    /// side calls `get_size` after every INSERT batch and compacts if the file
    /// exceeds the configured threshold (default: 5 MB).
    ///
    /// The default implementation returns 0 (no size information available).
    /// `OpfsStorage` overrides this with a real `FileSystemSyncAccessHandle.getSize()` call.
    /// Native disk backends don't need this — they use OS-level file metadata instead.
    fn get_size(&self) -> Result<u64, DbError> {
        Ok(0)
    }

    /// Stream log entries into state one at a time, without loading the full
    /// log into RAM. Implementations may load a binary snapshot first and only
    /// replay the delta lines written after the snapshot.
    ///
    /// The default implementation falls back to `read_log()` for backwards
    /// compatibility (used by WASM/EncryptedStorage which don't have snapshots).
    ///
    /// Returns the total number of entries processed.
    fn stream_log_into(&self, f: &mut dyn FnMut(LogEntry)) -> Result<u64, DbError> {
        // Default: load everything into a Vec, then iterate.
        // Concrete implementations (AsyncDiskStorage, SyncDiskStorage) override
        // this with a more efficient snapshot + streaming approach.
        let entries = self.read_log()?;
        let count = entries.len() as u64;
        for entry in entries {
            f(entry);
        }
        Ok(count)
    }
}

// ─── Startup replay ───────────────────────────────────────────────────────────
//
// When the server starts (or the WASM worker initialises), we need to rebuild
// the in-memory state from the persistent log. These functions handle that.
//
// The process is:
//   1. Call storage.stream_log_into() — this either loads a binary snapshot
//      + delta (fast path) or streams the full log line-by-line (slow path).
//   2. For each LogEntry, call apply_entry() to update the in-memory DashMaps.
//   3. After all entries are applied, the in-memory state matches the log.
// ─────────────────────────────────────────────────────────────────────────────

/// Drive startup by streaming all log entries from storage into the in-memory
/// state and index maps. Uses snapshot + delta replay when available.
///
/// `state`   — the main data store: collection name → (key → document value)
/// `indexes` — the index store: "collection:field" → (field value → set of keys)
///
/// Returns the total number of log entries processed.
pub fn stream_into_state(
    storage: &dyn StorageBackend,
    state: &DashMap<String, DashMap<String, Value>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
) -> Result<u64, DbError> {
    let mut count = 0u64;
    // stream_log_into calls our closure once per LogEntry.
    // The closure applies each entry to the in-memory state immediately,
    // so only one entry is in memory at a time (no full Vec in RAM).
    storage.stream_log_into(&mut |entry| {
        apply_entry(&entry, state, indexes);
        count += 1;
    })?;
    Ok(count)
}

/// Apply a single log entry to the in-memory state and indexes.
///
/// This is the core of the replay logic. Each entry type maps to a specific
/// mutation of the in-memory DashMaps:
///
///   INSERT → insert/overwrite a document in the collection
///   DELETE → remove a document from the collection
///   DROP   → remove an entire collection and its indexes
///   INDEX  → register an index slot (the index is populated by subsequent INSERTs)
fn apply_entry(
    entry: &LogEntry,
    state: &DashMap<String, DashMap<String, Value>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
) {
    match entry.cmd.as_str() {
        "INSERT" => {
            // Get or create the collection's inner DashMap.
            // `entry()` returns a reference to the existing entry or inserts a
            // new one — this is an atomic operation on DashMap.
            let col = state
                .entry(entry.collection.clone())
                .or_insert_with(DashMap::new);
            // Insert (or overwrite) the document at the given key.
            col.insert(entry.key.clone(), entry.value.clone());
            // Update any indexes that cover this collection.
            // index_doc() iterates all indexes whose name starts with
            // "collection:" and adds this key to the appropriate index entries.
            crate::engine::indexing::index_doc(indexes, &entry.collection, &entry.key, &entry.value);
        }
        "DELETE" => {
            // Only act if the collection exists.
            if let Some(col) = state.get(&entry.collection) {
                // Before removing the document, remove it from all indexes.
                // We need the old value to know which index entries to remove.
                if let Some(old_val) = col.get(&entry.key) {
                    crate::engine::indexing::unindex_doc(
                        indexes,
                        &entry.collection,
                        &entry.key,
                        old_val.value(),
                    );
                }
                // Remove the document from the collection.
                col.remove(&entry.key);
            }
        }
        "DROP" => {
            // Remove the entire collection from the state map.
            state.remove(&entry.collection);
            // Remove all indexes that belong to this collection.
            // retain() keeps only entries where the closure returns true.
            // We drop any index whose key starts with "collection:" (e.g. "users:role").
            indexes.retain(|k, _| !k.starts_with(&format!("{}:", entry.collection)));
        }
        "INDEX" => {
            // Register an empty index slot for "collection:field".
            // The index will be populated as subsequent INSERT entries are applied.
            // `entry.key` holds the field name (e.g. "role" for "users:role").
            indexes.insert(
                format!("{}:{}", entry.collection, entry.key),
                DashMap::new(),
            );
        }
        // Unknown command types are silently ignored for forward compatibility.
        // If a future version of MoltenDB adds a new command, older versions
        // will simply skip those entries rather than crashing.
        _ => {}
    }
}

/// Replay a slice of already-decoded log entries into RAM state.
///
/// This is an alternative to stream_into_state() used when the entries have
/// already been loaded into memory (e.g. after decryption by EncryptedStorage).
/// It applies the same logic as apply_entry() but iterates a pre-built slice.
pub fn replay_log_entries(
    entries: &[LogEntry],
    state: &DashMap<String, DashMap<String, Value>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
) {
    for entry in entries {
        match entry.cmd.as_str() {
            "INSERT" => {
                // Get or create the collection, then insert the document.
                let col = state
                    .entry(entry.collection.clone())
                    .or_insert_with(DashMap::new);
                col.insert(entry.key.clone(), entry.value.clone());
                // Keep indexes in sync with the inserted document.
                crate::engine::indexing::index_doc(indexes, &entry.collection, &entry.key, &entry.value);
            }
            "DELETE" => {
                if let Some(col) = state.get(&entry.collection) {
                    // Remove from indexes before removing from state.
                    if let Some(old_val) = col.get(&entry.key) {
                        crate::engine::indexing::unindex_doc(
                            indexes,
                            &entry.collection,
                            &entry.key,
                            old_val.value(),
                        );
                    }
                    col.remove(&entry.key);
                }
            }
            "DROP" => {
                // Remove the collection and all its associated indexes.
                state.remove(&entry.collection);
                indexes.retain(|k, _| !k.starts_with(&format!("{}:", entry.collection)));
            }
            "INDEX" => {
                // Register an empty index slot.
                indexes.insert(
                    format!("{}:{}", entry.collection, entry.key),
                    DashMap::new(),
                );
            }
            _ => {}
        }
    }
    println!("✅ Database restored & Indexes rebuilt!");
}
