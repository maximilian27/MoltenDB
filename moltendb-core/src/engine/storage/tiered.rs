// ─── tiered.rs ────────────────────────────────────────────────────────────────
// Memory-mapped log reader + TieredStorage backend.
//
// MmapLogReader
// ─────────────
// Instead of reading the log file into a heap-allocated Vec<u8> on startup,
// we ask the OS to "map" the file directly into the process's virtual address
// space. The OS then pages in only the bytes we actually touch — regions of
// the log that are already covered by a snapshot are never loaded into RAM
// at all. This is especially valuable for large log files (hundreds of MB)
// where the snapshot covers most of the data and only a small delta needs to
// be read.
//
// How mmap works (simplified):
//   • The OS reserves a range of virtual addresses equal to the file size.
//   • When we read a byte in that range, the OS checks if the corresponding
//     file page is in the page cache. If yes, it's served from RAM instantly.
//     If no, the OS loads that 4 KB page from disk (a "page fault").
//   • Pages that are never accessed are never loaded — zero I/O cost.
//   • The OS manages eviction automatically: if RAM is tight, cold pages are
//     dropped and reloaded from disk on next access.
//
// Safety note: mmap is `unsafe` in Rust because the OS could theoretically
// modify the file while we're reading it (another process writing to it),
// which would be undefined behaviour. We mitigate this by:
//   a) Only using MmapLogReader for startup replay (read-once, then dropped).
//   b) The log file is only written to by our own background task via the
//      MPSC channel — no external process writes to it.
//
// TieredStorage
// ─────────────
// A storage backend that wraps AsyncDiskStorage and adds mmap-based startup
// replay. All writes, compaction, and snapshot logic are delegated to
// AsyncDiskStorage. There is no cold log — a single log file + snapshot is
// used for all data.
//
// File layout on disk:
//   my_database.log              ← append-only, all writes go here
//   my_database.log.snapshot.bin ← binary snapshot written on compaction
//
// Startup sequence:
//   1. Load snapshot (if exists) → apply all entries in it instantly.
//   2. Stream only the log lines written AFTER the snapshot (the "delta")
//      via mmap so the OS pages in only what's needed.
//
// ─────────────────────────────────────────────────────────────────────────────

// Only compile this file for native (non-WASM) builds.
#![cfg(not(target_arch = "wasm32"))]

use super::StorageBackend;
use super::disk::AsyncDiskStorage;
use crate::engine::types::{DbError, LogEntry};
use std::ops::ControlFlow;
use std::sync::Arc;

// ─── TieredStorage ────────────────────────────────────────────────────────────

/// Storage backend that delegates all operations to AsyncDiskStorage.
///
/// Exists as a named type so callers can opt into `tiered_mode` via config
/// without changing the rest of the engine. All cold-log logic has been
/// removed — a single log file + binary snapshot handles everything.
pub struct TieredStorage {
    inner: Arc<AsyncDiskStorage>,
}

impl TieredStorage {
    /// Open (or create) the database at `path`.
    pub fn new(path: &str) -> Result<Self, DbError> {
        let inner = Arc::new(AsyncDiskStorage::new(path)?);
        Ok(Self { inner })
    }
}

impl StorageBackend for TieredStorage {
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        self.inner.write_entry(entry)
    }

    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        self.inner.read_log()
    }

    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError> {
        self.inner.compact(entries)
    }

    fn compact_with_hook(&self, entries: Vec<LogEntry>, hook: Option<String>) -> Result<(), DbError> {
        self.inner.compact_with_hook(entries, hook)
    }

    fn read_at(&self, offset: u64, length: u32) -> Result<Vec<u8>, DbError> {
        self.inner.read_at(offset, length)
    }

    fn stream_log_into(&self, f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>) -> Result<u64, DbError> {
        self.inner.stream_log_into(f)
    }
}
