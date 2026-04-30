// ─── disk/async_storage.rs ───────────────────────────────────────────────────
//
// Design: the write path is completely non-blocking. When write_entry() is
// called, it serializes the entry to JSON and sends it over an unbounded MPSC
// channel. A background Tokio task receives from that channel and writes to a
// BufWriter. The BufWriter is flushed every 50 ms (on timeout) or whenever
// the channel is drained.
//
// Trade-off: if the process is killed (SIGKILL / power loss) within the 50 ms
// window, the last few writes may be lost. For analytics workloads this is
// usually acceptable. Use SyncDiskStorage if you need zero data loss.
// ─────────────────────────────────────────────────────────────────────────────

use super::super::StorageBackend;
use super::log::{write_compacted_log_no_tx, stream_log_entries, read_log_from_disk};
use super::snapshot::{write_snapshot, load_snapshot, snapshot_path};
use crate::engine::types::{DbError, LogEntry};
use std::fs::OpenOptions;
use std::ops::ControlFlow;
use std::io::{BufWriter, Write};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// High-performance async disk writer.
///
/// Writes are sent over an MPSC channel and flushed to disk every 50 ms by a
/// background Tokio task. The write path never blocks the caller.
pub struct AsyncDiskStorage {
    /// The sending half of the MPSC channel. Cloning this is cheap — all
    /// clones share the same underlying channel.
    sender: Option<mpsc::UnboundedSender<String>>,
    /// Path to the log file on disk. Stored so we can read/compact it later.
    path: String,
    /// Handle to the background writer task. Stored so Drop can await it.
    writer_task: Option<JoinHandle<()>>,
}

impl AsyncDiskStorage {
    /// Open (or create) the log file at `path` and spawn the background writer task.
    pub fn new(path: &str) -> Result<Self, DbError> {
        // Create an unbounded MPSC channel.
        // `log_tx` (sender) is kept in the struct; `log_rx` (receiver) goes to the task.
        let (log_tx, mut log_rx) = mpsc::unbounded_channel::<String>();
        let path_clone = path.to_string();

        // Spawn a Tokio task that owns the file handle and BufWriter.
        // This task runs for the lifetime of the server.
        let writer_task = tokio::spawn(async move {
            // Open the file in append mode so existing data is preserved.
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path_clone)
                .unwrap();
            let mut w = BufWriter::new(file);

            loop {
                // Wait up to 50 ms for the next message.
                // If a message arrives within 50 ms → process it immediately.
                // If the timeout fires → flush the BufWriter to disk.
                match tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    log_rx.recv(),
                )
                .await
                {
                    // A message arrived within the timeout window.
                    Ok(Some(log_line)) => {
                        // Special sentinel: the compact() method sends this to
                        // tell us to swap the log file atomically.
                        if log_line.starts_with("__RELOAD_FILE__") {
                            // Extract the temp file path from the sentinel string.
                            let temp_path = log_line.replace("__RELOAD_FILE__", "");
                            // println!("🔥 Worker: Reloading file from {}", temp_path);

                            // Flush and close the current file before renaming.
                            // On Windows, a file cannot be renamed while it's open.
                            w.flush().unwrap();
                            drop(w); // Release the file handle / Windows lock

                            // Atomically replace the live log with the compacted version.
                            if let Err(e) = std::fs::rename(&temp_path, &path_clone) {
                                tracing::error!("Failed to swap compacted file: {}", e);
                            }

                            // Re-open the (now compacted) log file for future writes.
                            let new_file = OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&path_clone)
                                .unwrap();
                            w = BufWriter::new(new_file);
                        } else {
                            // Normal log line — append it to the BufWriter's buffer.
                            if let Err(e) = writeln!(w, "{}", log_line) {
                                tracing::error!("Failed to write to disk: {}", e);
                            }
                        }
                    }
                    // The channel was closed (sender dropped) — the server is shutting down.
                    // The BufWriter will be dropped here, which flushes its buffer to the OS.
                    Ok(None) => break,
                    // Timeout fired — no message in the last 50 ms. Flush buffered data.
                    Err(_) => {
                        let _ = w.flush();
                    }
                }
            }
            // When the loop exits, `w` is dropped here, which flushes the BufWriter.
            let _ = w.flush();
        });

        Ok(Self {
            sender: Some(log_tx),
            path: path.to_string(),
            writer_task: Some(writer_task),
        })
    }
}

impl Drop for AsyncDiskStorage {
    /// On drop, close the sender (signals the writer task to exit) then block
    /// until the task has drained its queue and flushed everything to disk.
    fn drop(&mut self) {
        // Drop the sender — this closes the channel and causes log_rx.recv()
        // to return None, which breaks the writer task's loop.
        drop(self.sender.take());

        // Now await the writer task so we don't return until all queued lines
        // have been written and flushed to the OS.
        if let Some(handle) = self.writer_task.take() {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(handle)
            })
            .ok();
        }
    }
}

impl StorageBackend for AsyncDiskStorage {
    /// Serialize `entry` to a JSON string and send it to the background writer.
    /// This call returns immediately — it never blocks waiting for disk I/O.
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        let json_line = serde_json::to_string(entry)?;
        // send() only fails if the receiver (background task) has been dropped,
        // which means the server is shutting down.
        if let Some(ref sender) = self.sender {
            sender.send(json_line).map_err(|_| DbError::WriteError)?;
        }
        Ok(())
    }

    /// Read all entries from the log file into a Vec.
    /// Used by EncryptedStorage which needs the full list to decrypt.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        read_log_from_disk(&self.path)
    }

    /// Compact the log: write a binary snapshot, rewrite the log to be empty,
    /// then signal the background task to swap the file.
    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError> {
        self.compact_with_hook(entries, None)
    }

    /// Internal compact implementation that can take a post-backup script.
    fn compact_with_hook(&self, entries: Vec<LogEntry>, hook: Option<String>) -> Result<(), DbError> {
        // Step 1: Write a binary snapshot.
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
        // Since the snapshot now contains the full state, we can start the log fresh.
        let temp_path = format!("{}.tmp", self.path);
        write_compacted_log_no_tx(&temp_path, &[])?;

        // Step 3: Send the sentinel to the background task so it flushes,
        // closes the current file, renames the temp file over it, and reopens.
        if let Some(ref sender) = self.sender {
            sender
                .send(format!("__RELOAD_FILE__{}", temp_path))
                .map_err(|_| DbError::WriteError)?;
        }
        Ok(())
    }

    /// Read exactly `length` bytes from the log at `offset`.
    fn read_at(&self, offset: u64, length: u32) -> Result<Vec<u8>, DbError> {
        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0u8; length as usize];
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    /// Stream log entries into state using snapshot + delta replay.
    ///
    /// Fast path (after first compaction):
    ///   1. Load binary snapshot → apply all entries in it.
    ///   2. Stream only the log lines written AFTER the snapshot (the "delta").
    ///
    /// Slow path (first run, no snapshot):
    ///   Stream the entire log file line-by-line. No full Vec in RAM.
    fn stream_log_into(
        &self,
        f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>,
    ) -> Result<u64, DbError> {
        let mut count = 0u64;
        // Attempt to load the binary snapshot for fast startup.
        if let Some((snapshot_entries, seq)) = load_snapshot(&self.path) {
            for entry in snapshot_entries {
                // Entries from snapshot MUST be Hot because they are not in the log file
                // and thus don't have a valid RecordPointer for this log instance.
                if let ControlFlow::Break(_) = f(entry, 0) {
                    return Ok(count);
                }
                count += 1;
            }
            // Then replay only the log lines that came after the snapshot.
            // `seq` is the number of lines to skip (already in the snapshot).
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

        // No snapshot found — stream the full log from the beginning.
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
