// ─── disk/log.rs ─────────────────────────────────────────────────────────────
//
// The log file stores LogEntry records in MessagePack format, length-prefixed:
//   [4 bytes LE] payload length
//   [N bytes]    rmp-serde encoded LogEntry
//
// On read, JSON lines are also accepted as a fallback so that existing databases
// written with the old format are replayed correctly on first startup after the
// upgrade. After the next compaction the log will be fully MessagePack.
// ─────────────────────────────────────────────────────────────────────────────

use crate::engine::types::{DbError, LogEntry};
use std::fs::{File, OpenOptions};
use std::ops::ControlFlow;
use std::io::{BufWriter, Read, Write};

pub fn write_compacted_log_no_tx(path: &str, entries: &[LogEntry]) -> Result<(), DbError> {
    let temp_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    let mut w = BufWriter::new(temp_file);
    for entry in entries {
        write_msgpack_entry(&mut w, entry)?;
    }
    w.flush()?;
    Ok(())
}

/// Encode one entry as a 4-byte LE length prefix followed by MessagePack bytes.
fn write_msgpack_entry<W: Write>(w: &mut W, entry: &LogEntry) -> Result<(), DbError> {
    let encoded = rmp_serde::to_vec(entry).map_err(|_| DbError::WriteError)?;
    let len = encoded.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&encoded)?;
    Ok(())
}


/// Stream all log entries, calling `f` for each one.
/// Skips the first `skip_lines` entries (already covered by a loaded snapshot).
/// Supports both the new MessagePack length-prefixed format and the legacy
/// JSON-lines format so existing databases are replayed correctly.
pub fn stream_log_entries<F>(path: &str, skip_lines: u64, mut f: F) -> Result<ControlFlow<(), ()>, DbError>
where
    F: FnMut(LogEntry, u32) -> ControlFlow<(), ()>,
{
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(ControlFlow::Continue(())), // first run, no log yet
    };

    // Peek at the first byte to detect format.
    // MessagePack entries start with a 4-byte length prefix; the first byte of
    // a valid length will never be `{` (0x7B) which is how JSON lines start.
    let mut buf = Vec::new();
    {
        let mut reader = std::io::BufReader::new(&file);
        reader.read_to_end(&mut buf)?;
    }

    if buf.is_empty() {
        return Ok(ControlFlow::Continue(()));
    }

    // Detect format by first byte.
    if buf[0] == b'{' || buf[0] == b'\n' {
        // Legacy JSON-lines format.
        let text = String::from_utf8_lossy(&buf);
        let mut idx: u64 = 0;
        for line in text.lines() {
            if line.trim().is_empty() { continue; }
            if idx < skip_lines { idx += 1; continue; }
            let len = line.len() as u32;
            if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
                if let ControlFlow::Break(_) = f(entry, len) {
                    return Ok(ControlFlow::Break(()));
                }
            }
            idx += 1;
        }
    } else {
        // MessagePack length-prefixed format.
        let mut pos = 0usize;
        let mut idx: u64 = 0;
        while pos + 4 <= buf.len() {
            let len = u32::from_le_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3]]) as usize;
            pos += 4;
            if pos + len > buf.len() { break; }
            let payload = &buf[pos..pos + len];
            pos += len;
            if idx < skip_lines { idx += 1; continue; }
            if let Ok(entry) = rmp_serde::from_slice::<LogEntry>(payload) {
                if let ControlFlow::Break(_) = f(entry, (len + 4) as u32) {
                    return Ok(ControlFlow::Break(()));
                }
            }
            idx += 1;
        }
    }

    Ok(ControlFlow::Continue(()))
}

/// Read all log entries into a Vec. Used by EncryptedStorage.
pub fn read_log_from_disk(path: &str) -> Result<Vec<LogEntry>, DbError> {
    let mut entries = Vec::new();
    let _ = stream_log_entries(path, 0, |e, _| {
        entries.push(e);
        ControlFlow::Continue(())
    })?;
    Ok(entries)
}
