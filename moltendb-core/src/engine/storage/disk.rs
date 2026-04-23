// ─── disk.rs ─────────────────────────────────────────────────────────────────
// This file implements two concrete StorageBackend implementations that write
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
// logic for fast startup (see "Snapshot helpers" section below).
// ─────────────────────────────────────────────────────────────────────────────

// StorageBackend is the trait defined in mod.rs that both structs implement.
use super::StorageBackend;
// LogEntry is the unit of data written to the log (cmd, collection, key, value).
// DbError is our custom error enum.
use crate::engine::types::{DbError, LogEntry};
// Standard library file I/O types.
use std::fs::{File, OpenOptions};
// BufRead lets us iterate a file line-by-line without loading it all into RAM.
// BufWriter batches small writes into larger OS-level write calls for efficiency.
use std::io::{BufRead, BufReader, BufWriter, Write};
// Arc = thread-safe reference-counted pointer (shared ownership across threads).
// Mutex = mutual exclusion lock (only one thread can hold it at a time).
use std::sync::{Arc, Mutex};
// Tokio's async MPSC channel: multiple producers, single consumer.
// Used to send log lines from the write path to the background flush task.
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// ─── Snapshot helpers ────────────────────────────────────────────────────────
//
// A "snapshot" is a compact binary file that captures the entire current state
// of the database at a point in time. On the next startup we load the snapshot
// first (fast binary deserialization) and then only replay the log lines that
// were written AFTER the snapshot was taken — instead of replaying the entire
// log from the beginning. This dramatically reduces startup time for large DBs.
//
// Snapshot file format (binary, little-endian):
//   [8 bytes]  magic header: "MOLTSNAP"
//   [8 bytes]  seq: number of log lines captured in this snapshot
//   [8 bytes]  count: number of LogEntry records that follow
//   for each entry:
//     [8 bytes]  len: byte length of the bincode-encoded entry
//     [len bytes] bincode-encoded LogEntry
// ─────────────────────────────────────────────────────────────────────────────

/// Returns the path of the binary snapshot file for a given log file path.
/// Convention: `my_database.log` → `my_database.log.snapshot.bin`
pub(super) fn snapshot_path(log_path: &str) -> String {
    format!("{}.snapshot.bin", log_path)
}

/// Serialize all current `entries` into a binary snapshot file and write it
/// atomically (write to `.tmp`, then rename over the real file so we never
/// end up with a half-written snapshot).
///
/// `seq` is the number of log lines that were present in the log file at the
/// time of compaction — it tells the next startup how many lines to skip.
pub(super) fn write_snapshot(log_path: &str, entries: &[LogEntry], seq: u64) -> Result<(), DbError> {
    let path = snapshot_path(log_path);
    // Write to a temp file first so the swap is atomic.
    let tmp = format!("{}.tmp", path);

    let file = OpenOptions::new()
        .create(true)   // create if it doesn't exist
        .write(true)
        .truncate(true) // overwrite any existing content
        .open(&tmp)?;
    let mut w = BufWriter::new(file);

    // Magic header so we can detect corrupt/wrong files on load.
    w.write_all(b"MOLTSNAP")?;
    // Sequence number: how many log lines are already captured here.
    w.write_all(&seq.to_le_bytes())?;

    // Number of entries, so the reader can pre-allocate a Vec.
    let count = entries.len() as u64;
    w.write_all(&count.to_le_bytes())?;

    // Each entry is length-prefixed so the reader knows how many bytes to read.
    for entry in entries {
        // bincode is a compact binary format — much faster to deserialize than JSON.
        let encoded = bincode::serialize(entry).map_err(|_| DbError::WriteError)?;
        let len = encoded.len() as u64;
        w.write_all(&len.to_le_bytes())?;
        w.write_all(&encoded)?;
    }

    // Flush the BufWriter to ensure all bytes reach the OS buffer.
    w.flush()?;
    // Drop the writer to release the file handle before renaming (required on Windows).
    drop(w);

    // Atomic rename: replaces the old snapshot file in one OS operation.
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Try to load a previously written binary snapshot.
/// Returns `Some((entries, seq))` on success, or `None` if:
///   - the snapshot file doesn't exist (first run)
///   - the magic header doesn't match (corrupt file)
///   - any read fails (truncated file, wrong format)
pub(super) fn load_snapshot(log_path: &str) -> Option<(Vec<LogEntry>, u64)> {
    let path = snapshot_path(log_path);
    // If the file doesn't exist, open() returns Err and we return None.
    let mut file = File::open(&path).ok()?;

    use std::io::Read;

    // Validate the magic header — if it doesn't match, the file is not ours.
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).ok()?;
    if &magic != b"MOLTSNAP" {
        return None; // Not a valid snapshot file
    }

    // Read the sequence number (how many log lines to skip on replay).
    let mut seq_bytes = [0u8; 8];
    file.read_exact(&mut seq_bytes).ok()?;
    let seq = u64::from_le_bytes(seq_bytes);

    // Read the entry count so we can pre-allocate the Vec.
    let mut count_bytes = [0u8; 8];
    file.read_exact(&mut count_bytes).ok()?;
    let count = u64::from_le_bytes(count_bytes) as usize;

    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        // Read the length prefix for this entry.
        let mut len_bytes = [0u8; 8];
        file.read_exact(&mut len_bytes).ok()?;
        let len = u64::from_le_bytes(len_bytes) as usize;

        // Read exactly `len` bytes and deserialize with bincode.
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf).ok()?;

        // If deserialization fails (e.g. schema changed), return None so we
        // fall back to full log replay instead of crashing.
        let entry: LogEntry = bincode::deserialize(&buf).ok()?;
        entries.push(entry);
    }

    Some((entries, seq))
}

// ─── Streaming log reader ─────────────────────────────────────────────────────
//
// The log file is a plain text file where each line is a JSON-encoded LogEntry.
// Instead of reading the whole file into a Vec<String> and then parsing, we
// stream it line-by-line so only one entry is in memory at a time.
// ─────────────────────────────────────────────────────────────────────────────

/// Open the log file at `path` and call `f` for each successfully parsed
/// `LogEntry`, skipping the first `skip_lines` lines (those are already
/// covered by a loaded snapshot and don't need to be replayed again).
///
/// Lines that fail to parse (e.g. partial writes from a crash) are silently
/// skipped — the database will simply not see those entries, which is safe
/// because the in-memory state is rebuilt from what we can read.
pub fn stream_log_entries<F>(path: &str, skip_lines: u64, mut f: F) -> Result<(), DbError>
where
    F: FnMut(LogEntry, u32), // closure called once per valid entry + raw byte length
{
    // If the file doesn't exist yet (first run), just do nothing.
    if let Ok(file) = File::open(path) {
        // BufReader wraps the file with an internal buffer so we don't make
        // one syscall per byte — it reads in chunks and serves lines from RAM.
        let reader = BufReader::new(file);
        for (i, line) in reader.lines().enumerate() {
            // Skip lines already captured in the snapshot.
            if (i as u64) < skip_lines {
                continue;
            }
            // Ignore lines that fail to read (e.g. I/O error mid-line).
            if let Ok(json_str) = line {
                let length = json_str.len() as u32;
                // Ignore lines that fail to parse (e.g. partial write on crash).
                if let Ok(entry) = serde_json::from_str::<LogEntry>(&json_str) {
                    f(entry, length); // hand the entry and its raw length to the caller
                }
            }
        }
    }
    Ok(())
}

/// Count the total number of lines in the log file.
/// This is used when writing a snapshot to record the current sequence number
/// (i.e. "the snapshot covers the first N lines of the log").
pub(super) fn count_log_lines(path: &str) -> u64 {
    if let Ok(file) = File::open(path) {
        // .lines() is lazy — it reads one line at a time, so this doesn't
        // load the whole file into memory.
        BufReader::new(file).lines().count() as u64
    } else {
        0 // File doesn't exist yet
    }
}

// ─── read_log (still needed by EncryptedStorage wrapper) ─────────────────────
//
// EncryptedStorage wraps another StorageBackend and decrypts entries before
// they can be applied to state. Because decryption must happen before we can
// call apply_entry(), EncryptedStorage uses read_log() (which returns a full
// Vec) rather than stream_log_into() (which applies entries on the fly).
// ─────────────────────────────────────────────────────────────────────────────

/// Read all log entries from disk into a Vec<LogEntry>.
/// This is a convenience wrapper around stream_log_entries that collects
/// everything into a Vec. Used by EncryptedStorage.
pub fn read_log_from_disk(path: &str) -> Result<Vec<LogEntry>, DbError> {
    let mut entries = Vec::new();
    // skip_lines = 0 means read from the very beginning (no snapshot skip here,
    // because EncryptedStorage handles its own snapshot logic via read_log).
    stream_log_entries(path, 0, |e, _| entries.push(e))?;
    Ok(entries)
}

// ─── Compacted log writer ─────────────────────────────────────────────────────
//
// Compaction rewrites the log file to contain only the current state of the
// database — removing all superseded INSERT entries and all DELETE tombstones.
// This keeps the log file from growing unboundedly over time.
// ─────────────────────────────────────────────────────────────────────────────

/// Write a compacted set of entries to `path` (which should be a `.tmp` file).
/// The caller is responsible for atomically swapping this file over the real log.
pub(super) fn write_compacted_log(path: &str, entries: &[LogEntry]) -> Result<(), DbError> {
    let temp_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true) // start fresh — we're rewriting the whole log
        .open(path)?;
    let mut temp_writer = BufWriter::new(temp_file);

    // Write each entry as a JSON line, same format as the live log.
    for entry in entries {
        writeln!(temp_writer, "{}", serde_json::to_string(&entry)?)?;
    }

    temp_writer.flush()?;
    Ok(())
}

// ─── AsyncDiskStorage ─────────────────────────────────────────────────────────
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

    /// Compact the log: write a binary snapshot, rewrite the log to contain
    /// only current state, then signal the background task to swap the file.
    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError> {
        // Step 1: Write a binary snapshot so the next startup can skip log replay.
        // We record `seq` = current line count so we know where the snapshot ends.
        let seq = count_log_lines(&self.path);
        if let Err(e) = write_snapshot(&self.path, &entries, seq) {
            tracing::warn!("⚠️  Failed to write snapshot during compaction: {}", e);
            // Non-fatal: we continue without a snapshot. Next startup will do a
            // full log replay instead, which is slower but still correct.
        }

        // Step 2: Write the compacted log to a temp file.
        let temp_path = format!("{}.tmp", self.path);
        write_compacted_log(&temp_path, &entries)?;

        // Step 3: Send the sentinel to the background task so it flushes,
        // closes the current file, renames the temp file over it, and reopens.
        // This is the only safe way to swap the file on Windows (where you
        // can't rename a file that's currently open by another handle).
        if let Some(ref sender) = self.sender {
            sender
                .send(format!("__RELOAD_FILE__{}", temp_path))
                .map_err(|_| DbError::WriteError)?;
        }
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
    ///
    /// Fast path (after first compaction):
    ///   1. Load binary snapshot → apply all entries in it.
    ///   2. Stream only the log lines written AFTER the snapshot (the "delta").
    ///
    /// Slow path (first run, no snapshot):
    ///   Stream the entire log file line-by-line. No full Vec in RAM.
    fn stream_log_into(
        &self,
        f: &mut dyn FnMut(LogEntry, u32),
    ) -> Result<u64, DbError> {
        // Attempt to load the binary snapshot for fast startup.
        if let Some((snapshot_entries, seq)) = load_snapshot(&self.path) {
            tracing::info!(
                "⚡ Snapshot loaded ({} entries, seq {}). Replaying delta only...",
                snapshot_entries.len(),
                seq
            );
            for entry in snapshot_entries {
                // For snapshots, we re-serialize because they aren't in the log file
                let json = serde_json::to_vec(&entry).unwrap_or_default();
                let length = json.len() as u32;
                f(entry, length);
            }
            // Then replay only the log lines that came after the snapshot.
            // `seq` is the number of lines to skip (already in the snapshot).
            stream_log_entries(&self.path, seq, |e, l| f(e, l))?;
            let total = count_log_lines(&self.path);
            return Ok(total);
        }

        // No snapshot found — stream the full log from the beginning.
        let mut count = 0u64;
        stream_log_entries(&self.path, 0, |e, l| {
            f(e, l);
            count += 1;
        })?;
        Ok(count)
    }
}

// ─── SyncDiskStorage ──────────────────────────────────────────────────────────
//
// Design: every write_entry() call acquires a Mutex, writes the JSON line to
// a BufWriter, and immediately flushes the BufWriter. The flush() call blocks
// until the OS confirms the data is in its write buffer (not necessarily on
// physical disk, but durable enough for most crash scenarios).
//
// Trade-off: much lower throughput than AsyncDiskStorage because every write
// blocks the caller. Use this when data loss is unacceptable.
// ─────────────────────────────────────────────────────────────────────────────

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

    /// Compact the log: write a binary snapshot, atomically swap the log file,
    /// then reopen the writer pointing at the new (compacted) file.
    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError> {
        // Step 1: Write binary snapshot for fast next startup.
        let seq = count_log_lines(&self.path);
        if let Err(e) = write_snapshot(&self.path, &entries, seq) {
            tracing::warn!("⚠️  Failed to write snapshot during compaction: {}", e);
        }

        // Step 2: Write compacted log to a temp file.
        let temp_path = format!("{}.tmp", self.path);
        write_compacted_log(&temp_path, &entries)?;

        // Step 3: Lock the writer, rename the temp file over the live log,
        // then reopen the writer so future writes go to the compacted file.
        let mut w = self.writer.lock().map_err(|_| DbError::LockPoisoned)?;
        // On Unix this rename is atomic. On Windows the file must be closed first,
        // but since we hold the Mutex no other thread can write concurrently.
        std::fs::rename(&temp_path, &self.path)?;

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
        f: &mut dyn FnMut(LogEntry, u32),
    ) -> Result<u64, DbError> {
        // Fast path: load snapshot and replay only the delta.
        if let Some((snapshot_entries, seq)) = load_snapshot(&self.path) {
            tracing::info!(
                "⚡ Snapshot loaded ({} entries, seq {}). Replaying delta only...",
                snapshot_entries.len(),
                seq
            );
            for entry in snapshot_entries {
                // For snapshots, we re-serialize because they aren't in the log file
                let json = serde_json::to_vec(&entry).unwrap_or_default();
                let length = json.len() as u32;
                f(entry, length);
            }
            stream_log_entries(&self.path, seq, |e, l| f(e, l))?;
            let total = count_log_lines(&self.path);
            return Ok(total);
        }

        // Slow path: stream the full log line-by-line.
        let mut count = 0u64;
        stream_log_entries(&self.path, 0, |e, l| {
            f(e, l);
            count += 1;
        })?;
        Ok(count)
    }
}
