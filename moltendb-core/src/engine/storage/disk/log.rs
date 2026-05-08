// ─── disk/log.rs ─────────────────────────────────────────────────────────────
//
// The log file is a plain text file where each line is a JSON-encoded LogEntry.
// Instead of reading the whole file into a Vec<String> and then parsing, we
// stream it line-by-line so only one entry is in memory at a time.
//
// This file also contains the compacted log writers used during compaction to
// rewrite the log file to contain only the current state of the database —
// removing all superseded INSERT entries and all DELETE tombstones. This keeps
// the log file from growing unboundedly over time.
// ─────────────────────────────────────────────────────────────────────────────

use crate::engine::types::{DbError, LogEntry};
use std::fs::{File, OpenOptions};
use std::ops::ControlFlow;
use std::io::{BufRead, BufReader, BufWriter, Write};

pub fn write_compacted_log_no_tx(path: &str, entries: &[LogEntry]) -> Result<(), DbError> {
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

pub fn write_compacted_log(path: &str, entries: &[LogEntry]) -> Result<(), DbError> {
    let temp_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true) // start fresh — we're rewriting the whole log
        .open(path)?;
    let mut temp_writer = BufWriter::new(temp_file);

    // Write each entry as a JSON line, same format as the live log.
    for entry in entries {
        // We write each entry in its own transaction in the compacted log.
        // This ensures they are replayed correctly even if followed by other log entries.
        let tx_id = format!("compact-{}", entry.key);
        
        let begin = LogEntry {
            cmd: "TX_BEGIN".to_string(),
            collection: entry.collection.clone(),
            key: tx_id.clone(),
            value: serde_json::Value::Null,
            _t: entry._t,
        };
        writeln!(temp_writer, "{}", serde_json::to_string(&begin)?)?;
        
        writeln!(temp_writer, "{}", serde_json::to_string(&entry)?)?;
        
        let commit = LogEntry {
            cmd: "TX_COMMIT".to_string(),
            collection: entry.collection.clone(),
            key: tx_id,
            value: serde_json::Value::Null,
            _t: entry._t,
        };
        writeln!(temp_writer, "{}", serde_json::to_string(&commit)?)?;
    }

    temp_writer.flush()?;
    Ok(())
}

/// Open the log file at `path` and call `f` for each successfully parsed
/// `LogEntry`, skipping the first `skip_lines` lines (those are already
/// covered by a loaded snapshot and don't need to be replayed again).
///
/// Lines that fail to parse (e.g. partial writes from a crash) are silently
/// skipped — the database will simply not see those entries, which is safe
/// because the in-memory state is rebuilt from what we can read.
pub fn stream_log_entries<F>(path: &str, skip_lines: u64, mut f: F) -> Result<ControlFlow<(), ()>, DbError>
where
    F: FnMut(LogEntry, u32) -> ControlFlow<(), ()>, // closure called once per valid entry + raw byte length
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
                if let Ok(entry) = serde_json::from_str::<LogEntry>(&json_str)
                    && let ControlFlow::Break(_) = f(entry, length) {
                        return Ok(ControlFlow::Break(()));
                    }
            }
        }
    }
    Ok(ControlFlow::Continue(()))
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
    let _ = stream_log_entries(path, 0, |e, _| {
        entries.push(e);
        ControlFlow::Continue(())
    })?;
    Ok(entries)
}
