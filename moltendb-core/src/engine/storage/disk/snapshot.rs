// ─── disk/snapshot.rs ────────────────────────────────────────────────────────
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

use crate::engine::types::{DbError, LogEntry};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::SystemTime;
use std::io::{BufWriter, Write};

/// Returns the path of the binary snapshot file for a given log file path.
/// Convention: `my_database.log` → `my_database.log.snapshot.bin`
pub fn snapshot_path(log_path: &str) -> String {
    format!("{}.snapshot.bin", log_path)
}

pub fn write_snapshot(log_path: &str, entries: &[LogEntry], seq: u64) -> Result<(), DbError> {
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
        // We use JSON for snapshots as well for now to avoid bincode issues with dynamic Value
        let encoded = serde_json::to_vec(entry).map_err(|_| DbError::WriteError)?;
        let len = encoded.len() as u64;
        w.write_all(&len.to_le_bytes())?;
        w.write_all(&encoded)?;
    }

    // Flush the BufWriter to ensure all bytes reach the OS buffer.
    w.flush()?;
    // Drop the writer to release the file handle before renaming (required on Windows).
    drop(w);

    // Before renaming the new snapshot, move the old one to the backup folder.
    if Path::new(&path).exists() {
        let log_dir = Path::new(log_path).parent().unwrap_or_else(|| Path::new("."));
        let backup_dir = log_dir.join("backup");
        
        // Ensure backup directory exists
        std::fs::create_dir_all(&backup_dir)?;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        
        let filename = Path::new(&path).file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("snapshot.bin");
        
        let backup_path = backup_dir.join(format!("{}.{}.bak", filename, now));
        
        // Move current snapshot to backup
        let _ = std::fs::rename(&path, &backup_path);
    }

    // Atomic rename: replaces the old snapshot file in one OS operation.
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Try to load a previously written binary snapshot.
/// Returns `Some((entries, seq))` on success, or `None` if:
///   - the snapshot file doesn't exist (first run)
///   - the magic header doesn't match (corrupt file)
///   - any read fails (truncated file, wrong format)
pub fn load_snapshot(log_path: &str) -> Option<(Vec<LogEntry>, u64)> {
    let path = snapshot_path(log_path);
    if !Path::new(&path).exists() {
        return None;
    }
    tracing::info!("🔍 Attempting to load snapshot from {}", path);
    // If the file doesn't exist, open() returns Err and we return None.
    let mut file = File::open(&path).ok()?;

    use std::io::Read;

    // Validate the magic header — if it doesn't match, the file is not ours.
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).ok()?;
    if &magic != b"MOLTSNAP" {
        tracing::warn!("❌ Invalid snapshot magic header");
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

    tracing::info!("📂 Snapshot header: seq={}, count={}", seq, count);

    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        // Read the length prefix for this entry.
        let mut len_bytes = [0u8; 8];
        if let Err(e) = file.read_exact(&mut len_bytes) {
             tracing::error!("❌ Failed to read entry {} length: {}", i, e);
             return None;
        }
        let len = u64::from_le_bytes(len_bytes) as usize;

        // Read exactly `len` bytes and deserialize with JSON.
        let mut buf = vec![0u8; len];
        if let Err(e) = file.read_exact(&mut buf) {
             tracing::error!("❌ Failed to read entry {} data: {}", i, e);
             return None;
        }

        // If the entry is all zeros or empty, it might be a partial write
        if len > 0 && buf.iter().all(|&b| b == 0) {
            tracing::error!("❌ Entry {} data is all zeros. Snapshot might be corrupt.", i);
            return None;
        }

        // If deserialization fails (e.g. schema changed), return None so we
        // fall back to full log replay instead of crashing.
        let entry: LogEntry = match serde_json::from_slice(&buf) {
            Ok(e) => e,
            Err(err) => {
                let sample = if buf.len() > 20 { &buf[..20] } else { &buf };
                tracing::error!(
                    "❌ Failed to deserialize entry {} (len {}): {}. Sample: {:?}. This usually happens if the snapshot was created with an older version of MoltenDB or is corrupt. Falling back to log replay.",
                    i, len, err, sample
                );
                return None;
            }
        };
        entries.push(entry);
    }

    Some((entries, seq))
}
