// ─── disk/mod.rs ─────────────────────────────────────────────────────────────
// This module implements two concrete StorageBackend implementations that write
// the database log to a real file on disk:
//
//   • AsyncDiskStorage  — high-throughput, non-blocking writes via an MPSC
//                         channel + background Tokio task. Writes are buffered
//                         in memory and flushed to disk every 50 ms. Ideal for
//                         analytics / high-write workloads where losing the last
//                         50 ms of data on a crash is acceptable.
//
//   • SyncDiskStorage   — every write blocks until the OS confirms the data is
//                         on disk (flush after every entry). Zero data loss on
//                         crash, but much lower throughput. Enabled by setting
//                         the WRITE_MODE=sync environment variable.
//
// Both implementations share the same binary snapshot + streaming log replay
// logic for fast startup (see snapshot.rs and log.rs).
// ─────────────────────────────────────────────────────────────────────────────

pub(super) mod snapshot;
pub(super) mod log;
pub(super) mod async_storage;
pub(super) mod sync_storage;

// Re-export the two public storage structs so the rest of the crate can use
// them via `storage::disk::AsyncDiskStorage` / `storage::disk::SyncDiskStorage`.
pub use async_storage::AsyncDiskStorage;
pub use sync_storage::SyncDiskStorage;

// Re-export helpers used by tiered.rs for its own compaction + snapshot logic.
pub use log::write_compacted_log;
pub use snapshot::write_snapshot;
