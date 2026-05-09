// ─── storage/memory.rs ────────────────────────────────────────────────────────
// A no-op storage backend that keeps all data exclusively in the RAM DashMap.
//
// When `--in-memory` is set, MoltenDB skips all disk I/O:
//   • write_entry()  → discards the entry (no-op)
//   • read_log()     → returns an empty Vec (nothing to replay on startup)
//   • compact()      → no-op
//   • read_at()      → returns an error (Cold documents never exist in this mode)
//
// This turns MoltenDB into a pure in-process cache — think Redis-like behaviour
// with the full MoltenDB query engine (filters, joins, indexes) on top.
//
// ⚠️  All data is lost when the process exits. This mode is intentional for:
//   • Ephemeral caches / session stores
//   • CI test environments that need a clean slate on every run
//   • High-throughput scenarios where durability is not required
// ─────────────────────────────────────────────────────────────────────────────

use crate::engine::types::{DbError, LogEntry};
use crate::engine::storage::StorageBackend;
use std::ops::ControlFlow;

/// A storage backend that holds no data on disk.
/// All writes are silently discarded; reads always return empty.
pub struct InMemoryStorage;

impl StorageBackend for InMemoryStorage {
    /// Discard the entry — nothing is written to disk.
    fn write_entry(&self, _entry: &LogEntry) -> Result<(), DbError> {
        Ok(())
    }

    /// Return an empty log — there is nothing to replay on startup.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        Ok(Vec::new())
    }


    /// Cold documents never exist in in-memory mode — every document is Hot.
    /// This should never be called, but returns a clear error if it is.
    fn read_at(&self, _offset: u64, _length: u32) -> Result<Vec<u8>, DbError> {
        Err(DbError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "read_at is not supported in --in-memory mode (no Cold documents exist)",
        )))
    }

    /// Stream log entries — always empty in in-memory mode.
    fn stream_log_into(&self, _f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>) -> Result<u64, DbError> {
        Ok(0)
    }
}
