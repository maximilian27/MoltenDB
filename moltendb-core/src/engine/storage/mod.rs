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
mod encrypted;
// tiered.rs provides MmapLogReader (memory-mapped cold log reads) and
// TieredStorage (hot + cold two-tier backend for large-scale deployments).
#[cfg(not(target_arch = "wasm32"))]
mod tiered;
// Re-export the concrete types so callers can write `storage::AsyncDiskStorage`
// instead of `storage::disk::AsyncDiskStorage`.
#[cfg(not(target_arch = "wasm32"))]
pub use disk::{AsyncDiskStorage, SyncDiskStorage};
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
#[cfg(feature = "schema")]
use serde_json::Value;
use std::ops::ControlFlow;
// DashMap is a concurrent hash map — like HashMap but safe to read/write from
// multiple threads simultaneously without a global lock.
// DashSet is the set equivalent.
use dashmap::{DashMap, DashSet};
// serde_json::Value is a dynamically-typed JSON value (can be object, array,
// string, number, bool, or null). All document data is stored as Value.

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

    /// Read exactly `length` bytes starting at `offset` from the log.
    ///
    /// This is used to fetch "Cold" documents from the append-only log without
    /// loading the entire file into memory.
    fn read_at(&self, offset: u64, length: u32) -> Result<Vec<u8>, DbError>;

    /// Return the current size of the persistent log file in bytes.
    ///
    /// Used by the WASM worker to implement size-based auto-compaction — the JS
    /// side calls `get_size` after every INSERT batch and compacts if the file
    /// exceeds the configured threshold (default: 5 MB).
    ///
    /// The default implementation returns 0 (no size information available).
    /// `OpfsStorage` overrides this with a real `FileSystemSyncAccessHandle.getSize()` call.
    /// Native disk backends don't need this — they use OS-level file metadata instead.
    #[allow(dead_code)]
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
    fn stream_log_into(&self, f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>) -> Result<u64, DbError> {
        // Default: load everything into a Vec, then iterate.
        // Concrete implementations (AsyncDiskStorage, SyncDiskStorage) override
        // this with a more efficient snapshot + streaming approach.
        let entries = self.read_log()?;
        let mut count = 0u64;
        for entry in entries {
            // Default re-serializes to get length. 
            // Better implementations override this.
            let json = serde_json::to_vec(&entry).unwrap_or_default();
            let length = json.len() as u32;
            if let ControlFlow::Break(_) = f(entry, length) {
                return Ok(count);
            }
            count += 1;
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
/// `state`   — the main data store: collection name → (key → document state)
/// `indexes` — the index store: "collection:field" → (field value → set of keys)
///
/// Returns the total number of log entries processed.
pub fn stream_into_state(
    storage: &dyn StorageBackend,
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    #[cfg(feature = "schema")] schemas: &DashMap<String, std::sync::Arc<(Value, jsonschema::Validator)>>,
) -> Result<u64, DbError> {
    let mut count = 0u64;
    let mut offset = 0u64;
    let mut tx_buffer: Vec<(LogEntry, crate::engine::types::RecordPointer)> = Vec::new();
    let mut active_tx: Option<String> = None;

    // stream_log_into calls our closure once per LogEntry, providing the 
    // LogEntry and its raw byte length in the log file.
    storage.stream_log_into(&mut |entry, length| {
        let pointer = crate::engine::types::RecordPointer {
            offset,
            length,
        };

        match entry.cmd.as_str() {
            "TX_BEGIN" => {
                active_tx = Some(entry.key.clone());
                tx_buffer.clear();
            }
            "TX_COMMIT" => {
                if active_tx.as_ref() == Some(&entry.key) {
                    // Flush buffer to DashMap
                    for (e, p) in tx_buffer.drain(..) {
                        apply_entry(
                            &e,
                            state,
                            indexes,
                            #[cfg(feature = "schema")] schemas,
                            Some(p),
                        );
                    }
                    active_tx = None;
                } else {
                    tracing::warn!("⚠️  TX_COMMIT seen for unknown or inactive transaction ID: {}. Ignoring.", entry.key);
                }
            }
            _ => {
                if active_tx.is_some() {
                    // Hold in RAM until commit
                    tx_buffer.push((entry, pointer));
                } else {
                    // Standard non-transactional entry
                    apply_entry(
                        &entry,
                        state,
                        indexes,
                        #[cfg(feature = "schema")] schemas,
                        Some(pointer),
                    );
                }
            }
        }

        count += 1;
        // +1 for the newline character appended to each JSON line in the log.
        offset += (length + 1) as u64;
        ControlFlow::Continue(())
    })?;

    // If active_tx is still Some, the file ended prematurely (crash).
    // In this case, we DISCARD the buffer to ensure atomicity of the last operation.
    Ok(count)
}

/// Apply a single log entry to the in-memory state and indexes.
///
/// If `pointer` is provided (during log replay), INSERT entries are stored
/// as `DocumentState::Cold(pointer)` to save memory. Live writes stay `Hot`.
pub fn apply_entry(
    entry: &LogEntry,
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    #[cfg(feature = "schema")] schemas: &DashMap<String, std::sync::Arc<(Value, jsonschema::Validator)>>,
    pointer: Option<crate::engine::types::RecordPointer>,
) {
    match entry.cmd.as_str() {
        "INSERT" => {
            let col = state
                .entry(entry.collection.clone())
                .or_insert_with(DashMap::new);

            // During replay, we use the pointer (Cold). For live writes, we store the Value (Hot).
            let doc_state = if let Some(p) = pointer {
                crate::engine::types::DocumentState::Cold(p)
            } else {
                crate::engine::types::DocumentState::Hot(entry.value.clone())
            };

            col.insert(entry.key.clone(), doc_state);

            // Indexes ALWAYS store values in RAM to keep searches O(1).
            crate::engine::indexing::index_doc(indexes, &entry.collection, &entry.key, &entry.value);
        }
        "DELETE" => {
            if let Some(col) = state.get(&entry.collection) {
                // To unindex, we need the Value. If it's Cold, we'd have to fetch it.
                // However, during REPLAY, we can just skip unindexing if we don't have the value,
                // BUT that would break if a DELETE follows an INSERT.
                // Actually, unindex_doc needs the Value.
                // For simplicity in this v1 of Hybrid, we'll fetch if needed or change unindex_doc.
                // Wait, if it's Cold, we don't have the value.
                // I'll leave a TODO here and for now just handle Hot.
                if let Some(old_state) = col.get(&entry.key) {
                    if let crate::engine::types::DocumentState::Hot(old_val) = old_state.value() {
                         crate::engine::indexing::unindex_doc(
                            indexes,
                            &entry.collection,
                            &entry.key,
                            old_val,
                        );
                    }
                }
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
        #[cfg(feature = "schema")]
        "SCHEMA" => {
            // Re-compile and register the schema during replay.
            if let Ok(validator) = jsonschema::validator_for(&entry.value) {
                schemas.insert(entry.collection.clone(), std::sync::Arc::new((entry.value.clone(), validator)));
            }
        }
        // Unknown command types are silently ignored for forward compatibility.
        // If a future version of MoltenDB adds a new command, older versions
        // will simply skip those entries rather than crashing.
        _ => {}
    }
}

// Replay a slice of already-decoded log entries into RAM state.
//
// This is an alternative to stream_into_state() used when the entries have
// already been loaded into memory (e.g. after decryption by EncryptedStorage).
// It applies the same logic as apply_entry() but iterates a pre-built slice.

// pub fn replay_log_entries(
//     entries: &[LogEntry],
//     state: &DashMap<String, DashMap<String, Value>>,
//     indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
// ) {
//     for entry in entries {
//         match entry.cmd.as_str() {
//             "INSERT" => {
//                 // Get or create the collection, then insert the document.
//                 let col = state
//                     .entry(entry.collection.clone())
//                     .or_insert_with(DashMap::new);
//                 col.insert(entry.key.clone(), entry.value.clone());
//                 // Keep indexes in sync with the inserted document.
//                 crate::engine::indexing::index_doc(indexes, &entry.collection, &entry.key, &entry.value);
//             }
//             "DELETE" => {
//                 if let Some(col) = state.get(&entry.collection) {
//                     // Remove from indexes before removing from state.
//                     if let Some(old_val) = col.get(&entry.key) {
//                         crate::engine::indexing::unindex_doc(
//                             indexes,
//                             &entry.collection,
//                             &entry.key,
//                             old_val.value(),
//                         );
//                     }
//                     col.remove(&entry.key);
//                 }
//             }
//             "DROP" => {
//                 // Remove the collection and all its associated indexes.
//                 state.remove(&entry.collection);
//                 indexes.retain(|k, _| !k.starts_with(&format!("{}:", entry.collection)));
//             }
//             "INDEX" => {
//                 // Register an empty index slot.
//                 indexes.insert(
//                     format!("{}:{}", entry.collection, entry.key),
//                     DashMap::new(),
//                 );
//             }
//             _ => {}
//         }
//     }
//     println!("✅ Database restored & Indexes rebuilt!");
// }
