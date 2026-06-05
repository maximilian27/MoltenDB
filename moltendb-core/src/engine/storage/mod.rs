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
mod memory;
// Re-export the concrete types so callers can write `storage::AsyncDiskStorage`
// instead of `storage::disk::AsyncDiskStorage`.
#[cfg(not(target_arch = "wasm32"))]
pub use disk::{AsyncDiskStorage, SyncDiskStorage};
pub use encrypted::EncryptedStorage;
pub use memory::InMemoryStorage;

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
use dashmap::DashMap;
#[cfg(feature = "schema")]
use serde_json::Value;
use std::ops::ControlFlow;
use std::sync::Arc;
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

    /// Compact directly from the in-memory DashMaps, bypassing `LogEntry` allocation.
    /// Disk backends override this to call `write_snapshot_from_maps` which serializes
    /// each document inline — peak RAM stays at ~1x instead of ~2x.
    /// The default falls back to the iterator path (used by memory/encrypted/wasm backends).
    #[cfg(not(feature = "schema"))]
    fn compact_from_maps(
        &self,
        _state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    ) -> Result<(), DbError> {
        Ok(())
    }

    #[cfg(feature = "schema")]
    fn compact_from_maps(
        &self,
        state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
        schemas: &DashMap<String, std::sync::Arc<(Value, jsonschema::Validator)>>,
    ) -> Result<(), DbError> {
        let doc_count: u64 = state.iter().map(|c| c.value().len() as u64).sum();
        let count = doc_count + schemas.len() as u64;
        let doc_iter = state.iter().flat_map(|col_ref| {
            let col_name = col_ref.key().clone();
            col_ref
                .value()
                .iter()
                .filter_map(move |item_ref| {
                    let value: Value = rmp_serde::from_slice(item_ref.value()).ok()?;
                    Some(LogEntry::new(
                        "INSERT".to_string(),
                        col_name.to_string(),
                        item_ref.key().clone(),
                        value,
                    ))
                })
                .collect::<Vec<_>>()
        });
        let schema_iter = schemas
            .iter()
            .map(|schema_ref| {
                let (schema_json, _) = &**schema_ref.value();
                LogEntry::new(
                    "SCHEMA".to_string(),
                    schema_ref.key().to_string(),
                    "".to_string(),
                    schema_json.clone(),
                )
            })
            .collect::<Vec<_>>()
            .into_iter();
        let _ = (count, doc_iter.chain(schema_iter));
        Ok(())
    }

    /// Truncate the persistent store to 0 bytes and release any exclusive file
    /// handles so the caller can delete the underlying file/directory.
    ///
    /// Only meaningful for OPFS-backed storage — all other backends return Ok(())
    /// without doing anything. After this call the storage instance must not be
    /// used for further reads or writes.
    fn clear_opfs(&self) -> Result<(), DbError> {
        Ok(())
    }

    /// Return a short string identifying the storage backend in use.
    /// Defaults to `"inMemory"`. Disk backends override this.
    fn storage_mode(&self) -> &'static str {
        "inMemory"
    }

    /// Stream log entries into state one at a time, without loading the full
    /// log into RAM. Implementations may load a binary snapshot first and only
    /// replay the delta lines written after the snapshot.
    ///
    /// The default implementation falls back to `read_log()` for backwards
    /// compatibility (used by WASM/EncryptedStorage which don't have snapshots).
    ///
    /// Returns the total number of entries processed.
    fn stream_log_into(
        &self,
        f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>,
    ) -> Result<u64, DbError> {
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
/// state map. Uses snapshot + delta replay when available.
///
/// `state` — the main data store: collection name → (key → document state)
///
/// Returns the total number of log entries processed.
pub fn stream_into_state(
    storage: &dyn StorageBackend,
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    #[cfg(feature = "schema")] schemas: &DashMap<
        String,
        std::sync::Arc<(Value, jsonschema::Validator)>,
    >,
    ttl_expiry: &DashMap<String, u64>,
) -> Result<u64, DbError> {
    let mut count = 0u64;
    let mut tx_buffer: Vec<LogEntry> = Vec::new();
    let mut active_tx: Option<String> = None;

    storage.stream_log_into(&mut |entry, _length| {
        match entry.cmd.as_str() {
            "TX_BEGIN" => {
                active_tx = Some(entry.key.clone());
                tx_buffer.clear();
            }
            "TX_COMMIT" => {
                if active_tx.as_ref() == Some(&entry.key) {
                    for e in tx_buffer.drain(..) {
                        apply_entry(
                            &e,
                            state,
                            #[cfg(feature = "schema")]
                            schemas,
                            ttl_expiry,
                        );
                    }
                    active_tx = None;
                } else {
                    tracing::warn!(
                        "⚠️  TX_COMMIT seen for unknown or inactive transaction ID: {}. Ignoring.",
                        entry.key
                    );
                }
            }
            _ => {
                if active_tx.is_some() {
                    tx_buffer.push(entry);
                } else {
                    apply_entry(
                        &entry,
                        state,
                        #[cfg(feature = "schema")]
                        schemas,
                        ttl_expiry,
                    );
                }
            }
        }

        count += 1;
        ControlFlow::Continue(())
    })?;

    // If active_tx is still Some, the file ended prematurely (crash) — discard buffer.
    Ok(count)
}

/// Apply a single log entry to the in-memory state.
pub fn apply_entry(
    entry: &LogEntry,
    state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
    #[cfg(feature = "schema")] schemas: &DashMap<
        String,
        std::sync::Arc<(Value, jsonschema::Validator)>,
    >,
    ttl_expiry: &DashMap<String, u64>,
) {
    match entry.cmd.as_str() {
        "INSERT" => {
            let col = state
                .entry(Arc::from(entry.collection.as_str()))
                .or_default();
            if let Ok(bytes) = rmp_serde::to_vec(&entry.value) {
                col.insert(entry.key.clone(), bytes.into_boxed_slice());
            }
        }
        "DELETE" => {
            if let Some(col) = state.get(entry.collection.as_str()) {
                col.remove(&entry.key);
            }
        }
        "DROP" => {
            state.remove(entry.collection.as_str());
            ttl_expiry.remove(entry.collection.as_str());
        }
        "TTL_EXPIRY" => {
            // Restore the expiry timestamp — but only if it's still in the future.
            // If the expiry has already passed, the collection is expired and we
            // evict it from state so stale documents don't survive a reload.
            if let Ok(expires_at) = entry.key.parse::<u64>() {
                let now = crate::engine::operations::ttl::now_ms();
                if expires_at > now {
                    ttl_expiry.insert(entry.collection.clone(), expires_at);
                } else {
                    // Already expired — evict the stale documents from state.
                    state.remove(entry.collection.as_str());
                    ttl_expiry.remove(entry.collection.as_str());
                }
            }
        }
        "INDEX" => {
            // Legacy INDEX entries are silently ignored — indexing has been removed.
        }
        #[cfg(feature = "schema")]
        "SCHEMA" => {
            // Re-compile and register the schema during replay.
            if let Ok(validator) = jsonschema::validator_for(&entry.value) {
                schemas.insert(
                    entry.collection.clone(),
                    std::sync::Arc::new((entry.value.clone(), validator)),
                );
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
