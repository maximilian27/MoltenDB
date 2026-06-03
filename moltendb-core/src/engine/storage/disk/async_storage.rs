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
use super::snapshot::{write_snapshot_from_maps, load_snapshot};
use dashmap::DashMap;
use serde_json::Value;
use crate::engine::types::{DbError, LogEntry};
use std::fs::OpenOptions;
use std::ops::ControlFlow;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Message sent over the writer channel.
/// Normal log lines are `Write(bytes)`; compaction sends `Compact` with a
/// shared condvar so `compact_with_hook` can block until the file swap is done.
enum WriterMsg {
    Write(Vec<u8>),
    Compact {
        temp_path: String,
        done: Arc<(Mutex<bool>, Condvar)>,
    },
}

/// High-performance async disk writer.
///
/// Writes are sent over an MPSC channel and flushed to disk every 50 ms by a
/// background Tokio task. The write path never blocks the caller.
///
/// If the background task encounters a fatal I/O error (e.g. disk full), it
/// sets `io_fault` to `true`. The engine checks this flag before every write
/// and rejects new writes with `DbError::StorageFault` while it is set.
pub struct AsyncDiskStorage {
    /// The sending half of the MPSC channel. Cloning this is cheap — all
    /// clones share the same underlying channel.
    sender: Option<mpsc::UnboundedSender<WriterMsg>>,
    /// Path to the log file on disk. Stored so we can read/compact it later.
    path: String,
    /// Handle to the background writer task. Stored so Drop can await it.
    writer_task: Option<JoinHandle<()>>,
    /// Circuit-breaker flag. Set to `true` by the background task on any fatal
    /// I/O error. Checked by the engine on every write to prevent silent data loss.
    pub io_fault: Arc<AtomicBool>,
}

impl AsyncDiskStorage {
    /// Open (or create) the log file at `path` and spawn the background writer task.
    pub fn new(path: &str) -> Result<Self, DbError> {
        // Remove any stale .tmp file left by a previous crash before compaction swap.
        let _ = std::fs::remove_file(format!("{}.tmp", path));
        // Eagerly create the log file (or verify it is accessible) before spawning
        // the background task. This surfaces I/O errors immediately to the caller
        // instead of silently swallowing them inside the async task.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| {
                tracing::error!("Failed to open log file '{}': {}", path, e);
                DbError::WriteError
            })?;
        // Create an unbounded MPSC channel.
        // `log_tx` (sender) is kept in the struct; `log_rx` (receiver) goes to the task.
        let (log_tx, mut log_rx) = mpsc::unbounded_channel::<WriterMsg>();
        let path_clone = path.to_string();
        let io_fault = Arc::new(AtomicBool::new(false));
        let fault_flag = Arc::clone(&io_fault);

        // Spawn a Tokio task that owns the file handle and BufWriter.
        // This task runs for the lifetime of the server.
        let writer_task = tokio::spawn(async move {
            // Open the file in append mode so existing data is preserved.
            let file = match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path_clone)
            {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("Failed to open log file '{}': {}", path_clone, e);
                    return;
                }
            };
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
                    Ok(Some(msg)) => match msg {
                        WriterMsg::Compact { temp_path, done } => {
                            // Flush and close the current file before renaming.
                            // On Windows, a file cannot be renamed while it's open.
                            if let Err(e) = w.flush() {
                                tracing::error!("Failed to flush log before compaction swap: {}", e);
                            }
                            drop(w); // Release the file handle / Windows lock

                            // Atomically replace the live log with the compacted version.
                            if let Err(e) = std::fs::rename(&temp_path, &path_clone) {
                                tracing::error!("Failed to swap compacted file: {}", e);
                                // Clean up the orphaned temp file so it doesn't persist.
                                let _ = std::fs::remove_file(&temp_path);
                            }

                            // Re-open the (now compacted) log file for future writes.
                            let new_file = match OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&path_clone)
                            {
                                Ok(f) => f,
                                Err(e) => {
                                    tracing::error!("Failed to reopen log file '{}' after compaction: {}", path_clone, e);
                                    return;
                                }
                            };
                            w = BufWriter::new(new_file);

                            // Signal compact_with_hook that the swap is complete.
                            let (lock, cvar) = &*done;
                            let mut finished = lock.lock().unwrap();
                            *finished = true;
                            cvar.notify_one();
                        }
                        WriterMsg::Write(bytes) => {
                            // MessagePack entry — write raw length-prefixed bytes.
                            if let Err(e) = w.write_all(&bytes) {
                                tracing::error!("Fatal disk write error — entering read-only mode: {}", e);
                                fault_flag.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                    },
                    // The channel was closed (sender dropped) — the server is shutting down.
                    // The BufWriter will be dropped here, which flushes its buffer to the OS.
                    Ok(None) => break,
                    // Timeout fired — no message in the last 50 ms. Flush buffered data.
                    Err(_) => {
                        if let Err(e) = w.flush() {
                            tracing::error!("Fatal disk flush error — entering read-only mode: {}", e);
                            fault_flag.store(true, Ordering::Relaxed);
                            break;
                        }
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
            io_fault,
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

impl AsyncDiskStorage {
    fn swap_log(&self) -> Result<(), DbError> {
        let temp_path = format!("{}.tmp", self.path);
        write_compacted_log_no_tx(&temp_path, &[])?;
        if let Some(ref sender) = self.sender {
            let done = Arc::new((Mutex::new(false), Condvar::new()));
            sender.send(WriterMsg::Compact { temp_path, done: Arc::clone(&done) })
                .map_err(|_| DbError::WriteError)?;
            tokio::task::block_in_place(|| {
                let (lock, cvar) = &*done;
                let mut finished = lock.lock().unwrap();
                while !*finished { finished = cvar.wait(finished).unwrap(); }
            });
        } else {
            // No async writer — rename directly on the calling thread.
            std::fs::rename(&temp_path, &self.path).map_err(|_| DbError::WriteError)?;
        }
        Ok(())
    }
}

impl StorageBackend for AsyncDiskStorage {
    /// Serialize `entry` to MessagePack and send it to the background writer.
    /// This call returns immediately — it never blocks waiting for disk I/O.
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        let encoded = rmp_serde::to_vec(entry).map_err(|_| DbError::WriteError)?;
        let len = (encoded.len() as u32).to_le_bytes();
        let mut bytes = Vec::with_capacity(4 + encoded.len());
        bytes.extend_from_slice(&len);
        bytes.extend_from_slice(&encoded);
        if let Some(ref sender) = self.sender {
            sender.send(WriterMsg::Write(bytes)).map_err(|_| DbError::WriteError)?;
        }
        Ok(())
    }

    /// Read all entries from the log file into a Vec.
    /// Used by EncryptedStorage which needs the full list to decrypt.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        read_log_from_disk(&self.path)
    }

    /// Override: write snapshot directly from DashMaps — no LogEntry allocation, no Value cloning.
    #[cfg(not(feature = "schema"))]
    fn compact_from_maps(&self, state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>, hook: Option<String>) -> Result<(), DbError> {
        if let Err(e) = write_snapshot_from_maps(&self.path, state, 0) {
            tracing::warn!("⚠️  Failed to write snapshot during compaction: {}", e);
        } else if let Some(script_path) = hook {
            self.run_backup_hook(script_path);
        }
        self.swap_log()
    }

    #[cfg(feature = "schema")]
    fn compact_from_maps(&self, state: &DashMap<Arc<str>, DashMap<String, Box<[u8]>>>, schemas: &DashMap<String, std::sync::Arc<(Value, jsonschema::Validator)>>) -> Result<(), DbError> {
        if let Err(e) = write_snapshot_from_maps(&self.path, state, schemas, 0) {
            tracing::warn!("⚠️  Failed to write snapshot during compaction: {}", e);
        } 
        self.swap_log()
    }

    fn storage_mode(&self) -> &'static str {
        "async"
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
        if let Some(seq) = load_snapshot(&self.path, &mut |entry| {
            // Entries from snapshot MUST be Hot because they are not in the log file
            // and thus don't have a valid RecordPointer for this log instance.
            let res = f(entry, 0);
            if let ControlFlow::Continue(_) = res {
                count += 1;
            }
            res
        }) {
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
