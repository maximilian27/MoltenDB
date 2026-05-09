// ─── storage/memory.rs ────────────────────────────────────────────────────────
// A no-op storage backend that keeps all data exclusively in the RAM DashMap.
//
// When `--in-memory` is set, MoltenDB skips all disk I/O:
//   • write_entry()  → discards the entry (no-op)
//   • read_log()     → returns an empty Vec (nothing to replay on startup)
//   • stream_log_into() → no-op, returns 0
//
// This turns MoltenDB into a pure in-process cache — think Redis-like behaviour
// with the full MoltenDB query engine (filters, joins, sort, pagination) on top.
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


    /// Stream log entries — always empty in in-memory mode.
    fn stream_log_into(&self, _f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>) -> Result<u64, DbError> {
        Ok(0)
    }
}
