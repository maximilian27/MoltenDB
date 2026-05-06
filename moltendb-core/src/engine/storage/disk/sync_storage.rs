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
use super::snapshot::{write_snapshot, load_snapshot, snapshot_path};
use crate::engine::types::{DbError, LogEntry};
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
        let file = OpenOptions::new().create(true).append(true).open(path)?;

        Ok(Self {
            writer: Arc::new(Mutex::new(BufWriter::new(file))),
            path: path.to_string(),
        })
    }
}

impl StorageBackend for SyncDiskStorage {
    /// Serialize `entry` to JSON, write it to the BufWriter, and flush immediately.
    /// This call blocks until the OS has accepted the data.
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        let json_line = serde_json::to_string(entry)?;
        // Lock the Mutex — only one thread can write at a time.
        let mut w = self.writer.lock().map_err(|_| DbError::LockPoisoned)?;
        writeln!(w, "{}", json_line)?;
        // Flush immediately so the data is durable before we return.
        w.flush()?;
        Ok(())
    }

    /// Read all entries from the log file into a Vec.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        read_log_from_disk(&self.path)
    }

    /// Compact the log: write a binary snapshot, swap the log file with an
    /// empty one, then reopen the writer.
    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError> {
        self.compact_with_hook(entries, None)
    }

    fn compact_with_hook(&self, entries: Vec<LogEntry>, hook: Option<String>) -> Result<(), DbError> {
        // Step 1: Write binary snapshot for fast next startup.
        // After compaction the log is reset to empty, so seq=0: all future log
        // lines written after this snapshot must be replayed from the start.
        let seq = 0u64;
        if let Err(e) = write_snapshot(&self.path, &entries, seq) {
            tracing::warn!("⚠️  Failed to write snapshot during compaction: {}", e);
        } else if let Some(script_path) = hook {
            // If snapshot was successful and we have a hook, execute it.
            let snapshot_path = snapshot_path(&self.path);
            let abs_snapshot_path = match std::fs::canonicalize(&snapshot_path) {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => snapshot_path,
            };

            // Execute in background
            tokio::spawn(async move {
                let res = if cfg!(target_os = "windows") {
                    tokio::process::Command::new("powershell")
                        .arg("-ExecutionPolicy")
                        .arg("Bypass")
                        .arg("-Command")
                        .arg(format!("& '{}' '{}'", script_path, abs_snapshot_path))
                        .output()
                        .await
                } else {
                    tokio::process::Command::new("sh")
                        .arg(script_path)
                        .arg(abs_snapshot_path)
                        .output()
                        .await
                };

                match res {
                    Ok(output) => {
                        if !output.status.success() {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            tracing::error!("❌ Post-backup hook failed: {}", stderr);
                        } else {
                            tracing::info!("✅ Post-backup hook executed successfully");
                        }
                    }
                    Err(e) => {
                        tracing::error!("❌ Failed to spawn post-backup hook: {}", e);
                    }
                }
            });
        }

        // Step 2: Write an empty compacted log to a temp file.
        let temp_path = format!("{}.tmp", self.path);
        write_compacted_log_no_tx(&temp_path, &[])?;

        // Step 3: Lock the writer, rename the temp file over the live log,
        // then reopen the writer so future writes go to the compacted file.
        let mut w = self.writer.lock().map_err(|_| DbError::LockPoisoned)?;
        // On Unix this rename is atomic. On Windows the file must be closed first,
        // but since we hold the Mutex no other thread can write concurrently.
        if let Err(e) = std::fs::rename(&temp_path, &self.path) {
             tracing::error!("Failed to swap compacted file: {}", e);
             return Err(DbError::from(e));
        }

        // Reopen the file so the writer points at the new compacted log.
        let new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        *w = BufWriter::new(new_file);
        Ok(())
    }

    /// Read exactly `length` bytes from the log at `offset`.
    fn read_at(&self, offset: u64, length: u32) -> Result<Vec<u8>, DbError> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0u8; length as usize];
        file.read_exact(&mut buffer)?;
        Ok(buffer)
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
