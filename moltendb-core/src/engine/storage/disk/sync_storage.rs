// ─── disk/sync_storage.rs ────────────────────────────────────────────────────
//
// Design: every write_entry() call acquires a Mutex, writes the JSON line to
// a BufWriter, and immediately flushes the BufWriter. The flush() call blocks
// until the OS confirms the data is in its write buffer (not necessarily on
// physical disk, but durable enough for most crash scenarios).
//
// Trade-off: much lower throughput than AsyncDiskStorage because every write
// blocks the caller. Use this when data loss is unacceptable.
// ─────────────────────────────────────────────────────────────────────────────

use super::super::StorageBackend;
use super::log::{write_compacted_log_no_tx, stream_log_entries, read_log_from_disk};
use super::snapshot::{write_snapshot_from_maps, load_snapshot, snapshot_path};
use crate::engine::types::{DbError, LogEntry};
use dashmap::DashMap;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::ops::ControlFlow;
use std::io::{BufWriter, Write};
use std::sync::{Arc, Mutex};

/// High-durability synchronous disk writer.
///
/// Every write is flushed to disk before returning. Zero data loss on crash,
/// but lower throughput than AsyncDiskStorage. Enable with WRITE_MODE=sync.
pub struct SyncDiskStorage {
    /// The BufWriter wrapped in a Mutex so multiple threads can write safely.
    /// Arc allows the struct to be cloned (shared across Axum handler threads).
    writer: Arc<Mutex<BufWriter<File>>>,
    /// Path to the log file. Stored for read/compact operations.
    path: String,
}

impl SyncDiskStorage {
    /// Open (or create) the log file at `path` in append mode.
    pub fn new(path: &str) -> Result<Self, DbError> {
        // Remove any stale .tmp file left by a previous crash before compaction swap.
        let _ = std::fs::remove_file(format!("{}.tmp", path));
        let file = OpenOptions::new().create(true).append(true).open(path)?;

        Ok(Self {
            writer: Arc::new(Mutex::new(BufWriter::new(file))),
            path: path.to_string(),
        })
    }
}

impl SyncDiskStorage {
    fn run_backup_hook(&self, script_path: String) {
        let snapshot_path = snapshot_path(&self.path);
        let abs_snapshot_path = match std::fs::canonicalize(&snapshot_path) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => snapshot_path,
        };
        tokio::spawn(async move {
            let res = if cfg!(target_os = "windows") {
                tokio::process::Command::new("powershell")
                    .arg("-ExecutionPolicy").arg("Bypass").arg("-Command")
                    .arg(format!("& '{}' '{}'", script_path, abs_snapshot_path))
                    .output().await
            } else {
                tokio::process::Command::new("sh")
                    .arg(script_path).arg(abs_snapshot_path)
                    .output().await
            };
            match res {
                Ok(output) if !output.status.success() => {
                    tracing::error!("❌ Post-backup hook failed: {}", String::from_utf8_lossy(&output.stderr));
                }
                Ok(_) => tracing::info!("✅ Post-backup hook executed successfully"),
                Err(e) => tracing::error!("❌ Failed to spawn post-backup hook: {}", e),
            }
        });
    }

    fn swap_log(&self) -> Result<(), DbError> {
        let temp_path = format!("{}.tmp", self.path);
        write_compacted_log_no_tx(&temp_path, &[])?;
        let mut w = self.writer.lock().map_err(|_| DbError::LockPoisoned)?;
        if let Err(e) = std::fs::rename(&temp_path, &self.path) {
            tracing::error!("Failed to swap compacted file: {}", e);
            let _ = std::fs::remove_file(&temp_path);
            return Err(DbError::from(e));
        }
        let new_file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        *w = BufWriter::new(new_file);
        Ok(())
    }
}

impl StorageBackend for SyncDiskStorage {
    /// Serialize `entry` to MessagePack, write it to the BufWriter, and flush immediately.
    /// This call blocks until the OS has accepted the data.
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        let encoded = rmp_serde::to_vec(entry).map_err(|_| DbError::WriteError)?;
        let len = (encoded.len() as u32).to_le_bytes();
        let mut w = self.writer.lock().map_err(|_| DbError::LockPoisoned)?;
        w.write_all(&len)?;
        w.write_all(&encoded)?;
        w.flush()?;
        Ok(())
    }

    /// Read all entries from the log file into a Vec.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        read_log_from_disk(&self.path)
    }

    #[cfg(not(feature = "schema"))]
    fn compact_from_maps(&self, state: &DashMap<String, DashMap<String, Value>>, hook: Option<String>) -> Result<(), DbError> {
        if let Err(e) = write_snapshot_from_maps(&self.path, state, 0) {
            tracing::warn!("⚠️  Failed to write snapshot during compaction: {}", e);
        } else if let Some(script_path) = hook {
            self.run_backup_hook(script_path);
        }
        self.swap_log()
    }

    #[cfg(feature = "schema")]
    fn compact_from_maps(&self, state: &DashMap<String, DashMap<String, Value>>, schemas: &DashMap<String, std::sync::Arc<(Value, jsonschema::Validator)>>, hook: Option<String>) -> Result<(), DbError> {
        if let Err(e) = write_snapshot_from_maps(&self.path, state, schemas, 0) {
            tracing::warn!("⚠️  Failed to write snapshot during compaction: {}", e);
        } else if let Some(script_path) = hook {
            self.run_backup_hook(script_path);
        }
        self.swap_log()
    }

    /// Stream log entries into state using snapshot + delta replay.
    /// Same logic as AsyncDiskStorage::stream_log_into — see that method for details.
    fn stream_log_into(
        &self,
        f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>,
    ) -> Result<u64, DbError> {
        let mut count = 0u64;
        // Fast path: load snapshot and replay only the delta.
        if let Some(seq) = load_snapshot(&self.path, &mut |entry| {
            // Entries from snapshot MUST be Hot because they are not in the log file
            // and thus don't have a valid RecordPointer for this log instance.
            let res = f(entry, 0);
            if let ControlFlow::Continue(_) = res {
                count += 1;
            }
            res
        }) {
            if let ControlFlow::Break(_) = stream_log_entries(&self.path, seq, |e, l| {
                let res = f(e, l);
                if let ControlFlow::Continue(_) = res {
                    count += 1;
                }
                res
            })? {
                return Ok(count);
            }
            return Ok(count);
        }

        // Slow path: stream the full log line-by-line.
        let _ = stream_log_entries(&self.path, 0, |e, l| {
            let res = f(e, l);
            if let ControlFlow::Continue(_) = res {
                count += 1;
            }
            res
        })?;
        Ok(count)
    }
}
