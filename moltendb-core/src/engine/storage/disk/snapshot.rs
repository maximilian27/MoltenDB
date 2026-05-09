// ─── disk/snapshot.rs ────────────────────────────────────────────────────────
//
// A "snapshot" is a compact binary file that captures the entire current state
// of the database at a point in time. On the next startup we load the snapshot
// first (fast binary deserialization) and then only replay the log lines that
// were written AFTER the snapshot was taken — instead of replaying the entire
// log from the beginning. This dramatically reduces startup time for large DBs.
//
// Snapshot file format (binary, little-endian, gzip-compressed body):
//   [8 bytes]  magic header: "MOLTSNG3"  ("MOLTSNG2" = legacy JSON body)
//   [8 bytes]  seq: number of log lines captured in this snapshot
//   [8 bytes]  count: number of LogEntry records that follow
//   --- everything below this point is gzip-compressed ---
//   for each entry:
//     [4 bytes]  len: byte length of the MessagePack-encoded entry
//     [len bytes] rmp-serde encoded LogEntry
// ─────────────────────────────────────────────────────────────────────────────

use crate::engine::types::{DbError, LogEntry};
use std::fs::{File, OpenOptions};
use std::ops::ControlFlow;
use std::path::Path;
use std::time::SystemTime;
use std::io::{BufWriter, Read, Write};
use flate2::Compression;
use flate2::write::GzEncoder;
use flate2::read::GzDecoder;

/// Returns the path of the binary snapshot file for a given log file path.
/// Convention: `my_database.log` → `my_database.log.snapshot.bin`
pub fn snapshot_path(log_path: &str) -> String {
    format!("{}.snapshot.bin", log_path)
}

pub fn write_snapshot<'a>(log_path: &str, count: u64, entries: impl Iterator<Item = &'a LogEntry>, seq: u64) -> Result<(), DbError> {
    let path = snapshot_path(log_path);
    // Write to a temp file first so the swap is atomic.
    let tmp = format!("{}.tmp", path);

    let file = OpenOptions::new()
        .create(true)   // create if it doesn't exist
        .write(true)
        .truncate(true) // overwrite any existing content
        .open(&tmp)?;
    // Write the uncompressed header first, then compress the body.
    let mut raw = BufWriter::new(file);

    // Magic header — "MOLTSNG3" signals gzip-compressed MessagePack body.
    raw.write_all(b"MOLTSNG3")?;
    // Sequence number: how many log lines are already captured here.
    raw.write_all(&seq.to_le_bytes())?;

    // Number of entries written into the compressed body.
    raw.write_all(&count.to_le_bytes())?;

    // Flush the header so the GzEncoder starts right after it.
    raw.flush()?;

    // Wrap the underlying file in a gzip encoder for the body.
    let file_inner = raw.into_inner().map_err(|_| DbError::WriteError)?;
    let mut gz = GzEncoder::new(BufWriter::new(file_inner), Compression::default());

    // Each entry is length-prefixed so the reader knows how many bytes to read.
    for entry in entries {
        let encoded = rmp_serde::to_vec(entry).map_err(|_| DbError::WriteError)?;
        let len = encoded.len() as u32;
        gz.write_all(&len.to_le_bytes())?;
        gz.write_all(&encoded)?;
    }

    // Finish the gzip stream and flush everything to disk.
    gz.finish().map_err(|_| DbError::WriteError)?;

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

/// Try to load a previously written binary snapshot, streaming entries directly
/// into the provided callback `f` without collecting them into an intermediate Vec.
///
/// Returns `Some(seq)` on success, or `None` if:
///   - the snapshot file doesn't exist (first run)
///   - the magic header doesn't match (corrupt file)
///   - any read fails (truncated file, wrong format)
///
/// If `f` returns `ControlFlow::Break`, iteration stops early and `None` is returned.
pub fn load_snapshot(
    log_path: &str,
    f: &mut dyn FnMut(LogEntry) -> ControlFlow<(), ()>,
) -> Option<u64> {
    let path = snapshot_path(log_path);
    if !Path::new(&path).exists() {
        return None;
    }
    tracing::info!("🔍 Attempting to load snapshot from {}", path);
    let mut file = File::open(&path).ok()?;

    // Validate the magic header.
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).ok()?;

    let use_msgpack = match &magic {
        b"MOLTSNG3" => true,
        b"MOLTSNG2" => {
            tracing::warn!("⚠️  Old JSON snapshot detected — falling back to log replay");
            return None;
        }
        b"MOLTSNAP" => {
            tracing::warn!("⚠️  Old uncompressed snapshot detected — falling back to log replay");
            return None;
        }
        _ => {
            tracing::warn!("❌ Invalid snapshot magic header");
            return None;
        }
    };
    let _ = use_msgpack; // always true for MOLTSNG3

    // Read the sequence number (how many log lines to skip on replay).
    let mut seq_bytes = [0u8; 8];
    file.read_exact(&mut seq_bytes).ok()?;
    let seq = u64::from_le_bytes(seq_bytes);

    // Read the entry count (written before the compressed body).
    let mut count_bytes = [0u8; 8];
    file.read_exact(&mut count_bytes).ok()?;
    let count = u64::from_le_bytes(count_bytes) as usize;

    tracing::info!("📂 Snapshot header: seq={}, count={}", seq, count);

    // Wrap the rest of the file in a gzip decoder.
    let mut gz = GzDecoder::new(file);

    for i in 0..count {
        // Read the length prefix for this entry.
        let mut len_bytes = [0u8; 4];
        if let Err(e) = gz.read_exact(&mut len_bytes) {
            tracing::error!("❌ Failed to read entry {} length: {}", i, e);
            return None;
        }
        let len = u32::from_le_bytes(len_bytes) as usize;

        // Read exactly `len` bytes and deserialize with MessagePack.
        let mut buf = vec![0u8; len];
        if let Err(e) = gz.read_exact(&mut buf) {
            tracing::error!("❌ Failed to read entry {} data: {}", i, e);
            return None;
        }

        // If the entry is all zeros or empty, it might be a partial write.
        if len > 0 && buf.iter().all(|&b| b == 0) {
            tracing::error!("❌ Entry {} data is all zeros. Snapshot might be corrupt.", i);
            return None;
        }

        // If deserialization fails (e.g. schema changed), return None so we
        // fall back to full log replay instead of crashing.
        let entry: LogEntry = match rmp_serde::from_slice(&buf) {
            Ok(e) => e,
            Err(err) => {
                let sample = if buf.len() > 20 { &buf[..20] } else { &buf };
                tracing::error!(
                    "❌ Failed to deserialize entry {} (len {}): {}. Sample: {:?}. Falling back to log replay.",
                    i, len, err, sample
                );
                return None;
            }
        };

        // Stream directly into the caller — no intermediate Vec.
        if let ControlFlow::Break(_) = f(entry) {
            return None;
        }
    }

    Some(seq)
}
