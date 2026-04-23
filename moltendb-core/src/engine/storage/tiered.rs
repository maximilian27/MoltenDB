// ─── tiered.rs ────────────────────────────────────────────────────────────────
// This file implements two complementary features for large-scale MoltenDB
// deployments:
//
//   1. Memory-mapped log reading (MmapLogReader)
//      Instead of reading the log file into a heap-allocated Vec<u8> on startup,
//      we ask the OS to "map" the file directly into the process's virtual address
//      space. The OS then pages in only the bytes we actually touch — cold regions
//      of the log that are already covered by a snapshot are never loaded into RAM
//      at all. This is especially valuable for large log files (hundreds of MB)
//      where the snapshot covers most of the data and only a small delta needs to
//      be read.
//
//      How mmap works (simplified):
//        • The OS reserves a range of virtual addresses equal to the file size.
//        • When we read a byte in that range, the OS checks if the corresponding
//          file page is in the page cache. If yes, it's served from RAM instantly.
//          If no, the OS loads that 4 KB page from disk (a "page fault").
//        • Pages that are never accessed are never loaded — zero I/O cost.
//        • The OS manages eviction automatically: if RAM is tight, cold pages are
//          dropped and reloaded from disk on next access.
//
//      Safety note: mmap is `unsafe` in Rust because the OS could theoretically
//      modify the file while we're reading it (another process writing to it),
//      which would be undefined behaviour. We mitigate this by:
//        a) Only using MmapLogReader for startup replay (read-once, then dropped).
//        b) The log file is only written to by our own background task via the
//           MPSC channel — no external process writes to it.
//
//   2. Tiered storage (TieredStorage)
//      Splits the database into two tiers:
//
//        HOT tier  — the active log file. All new writes go here. Kept small by
//                    frequent compaction. Fully in RAM (via AsyncDiskStorage's
//                    BufWriter + DashMap cache). Sub-millisecond reads.
//
//        COLD tier — an archived log file (`<name>.cold.log`) that holds older
//                    data that has been "promoted" from the hot tier. The cold
//                    log is read-only after promotion. It is read via mmap on
//                    startup so the OS can page in only what's needed.
//
//      Write path:  all writes go to the hot tier (AsyncDiskStorage).
//      Read path:   on startup, cold tier is replayed first (via mmap), then
//                   hot tier is replayed on top. Hot entries overwrite cold ones
//                   for the same key, so the final in-memory state is correct.
//      Promotion:   when the hot log exceeds HOT_TIER_MAX_BYTES (default 50 MB),
//                   compact() promotes the hot log to the cold tier and starts
//                   a fresh hot log. The cold tier accumulates over time and is
//                   compacted separately (less frequently).
//
//      This design means:
//        • Writes are always fast (hot tier is small, async, buffered).
//        • Startup is fast (cold tier is mmap'd, hot tier is small).
//        • Memory usage is bounded (cold tier is paged by the OS, not all in RAM).
//        • The cold tier can grow very large (GBs) without affecting write latency.
//
// ─────────────────────────────────────────────────────────────────────────────

// Only compile this file for native (non-WASM) builds.
#![cfg(not(target_arch = "wasm32"))]

// The StorageBackend trait that TieredStorage implements.
use super::StorageBackend;
// AsyncDiskStorage is the hot-tier writer — all new writes go through it.
// count_log_lines: counts lines in the hot log to record the snapshot sequence number.
// write_snapshot: writes a binary snapshot of the hot tier for fast next startup.
// write_compacted_log: writes a minimal compacted log to a temp file before swapping.
use super::disk::{AsyncDiskStorage, count_log_lines, write_snapshot, write_compacted_log};
// Our internal data types.
use crate::engine::types::{DbError, LogEntry};
// memmap2 provides safe(r) memory-mapped file access.
// Mmap = read-only memory map. MmapOptions = builder for creating maps.
use memmap2::{Mmap, MmapOptions};
// Standard file I/O.
use std::fs::{File, OpenOptions};
// BufRead lets us iterate a byte slice line-by-line without copying.
use std::io::{BufRead, BufReader, Cursor};
// Arc = thread-safe reference-counted pointer.
use std::sync::Arc;

// ─── Constants ────────────────────────────────────────────────────────────────

/// Maximum size of the hot log before it is promoted to the cold tier.
/// When compact() is called and the hot log exceeds this size, the hot log
/// is appended to the cold log and a fresh hot log is started.
/// 50 MB is a good default: small enough for fast startup replay, large enough
/// to avoid too-frequent promotions.
const HOT_TIER_MAX_BYTES: u64 = 50 * 1024 * 1024; // 50 MB

// ─── MmapLogReader ────────────────────────────────────────────────────────────

/// A read-only, memory-mapped view of a log file.
///
/// Used to replay the cold tier on startup without loading the entire file
/// into a heap-allocated buffer. The OS pages in only the bytes we read.
///
/// Lifetime: created at startup, used once for replay, then dropped.
/// Dropping the MmapLogReader releases the virtual address mapping.
pub struct MmapLogReader {
    /// The memory-mapped file contents.
    /// `Mmap` is a byte slice (`&[u8]`) backed by the OS page cache.
    /// Accessing bytes in it may trigger page faults (disk reads) if the
    /// corresponding file pages are not yet in RAM.
    mmap: Mmap,
}

impl MmapLogReader {
    /// Open the file at `path` and create a read-only memory map over it.
    ///
    /// Returns `None` if the file doesn't exist or is empty (nothing to replay).
    /// Returns `Some(reader)` if the file exists and has content.
    ///
    /// # Safety
    /// The `unsafe` block is required by memmap2 because the OS could modify
    /// the file while we hold the map. We accept this risk because:
    ///   - The log file is only written by our own background task.
    ///   - We use the map read-once during startup, then drop it immediately.
    pub fn open(path: &str) -> Option<Self> {
        // Try to open the file — if it doesn't exist, return None silently.
        let file = File::open(path).ok()?;

        // Check the file size — an empty file has nothing to map.
        let metadata = file.metadata().ok()?;
        if metadata.len() == 0 {
            return None;
        }

        // Create the memory map.
        // SAFETY: We only read from this map during startup replay, and the
        // log file is only appended to by our own controlled background task.
        let mmap = unsafe { MmapOptions::new().map(&file).ok()? };

        Some(Self { mmap })
    }

    /// Iterate over all valid LogEntry lines in the mapped file, calling `f`
    /// for each one. Lines that fail to parse are silently skipped (same
    /// behaviour as the streaming disk reader — tolerates partial writes).
    ///
    /// `skip_lines` allows skipping entries already covered by a snapshot,
    /// so we only replay the delta portion of the file.
    pub fn stream_entries<F>(&self, skip_lines: u64, mut f: F)
    where
        F: FnMut(LogEntry, u32),
    {
        // Wrap the mmap byte slice in a Cursor so we can use BufRead::lines().
        // Cursor<&[u8]> implements Read, and BufReader adds line-buffering.
        // This lets us iterate line-by-line without copying the whole file.
        let cursor = Cursor::new(&self.mmap[..]);
        let reader = BufReader::new(cursor);

        for (i, line) in reader.lines().enumerate() {
            // Skip lines already captured in the snapshot.
            if (i as u64) < skip_lines {
                continue;
            }
            // Ignore I/O errors (shouldn't happen with mmap, but be safe).
            if let Ok(json_str) = line {
                let length = json_str.len() as u32;
                // Ignore lines that fail to parse (partial writes on crash).
                if let Ok(entry) = serde_json::from_str::<LogEntry>(&json_str) {
                    f(entry, length);
                }
            }
        }
    }

    /// Return the total number of lines in the mapped file.
    /// Used to record the sequence number when writing a snapshot.
    pub fn line_count(&self) -> u64 {
        let cursor = Cursor::new(&self.mmap[..]);
        BufReader::new(cursor).lines().count() as u64
    }
}

// ─── TieredStorage ────────────────────────────────────────────────────────────

/// Two-tier storage backend: hot (active writes) + cold (archived, mmap-read).
///
/// All writes go to the hot tier. On startup, the cold tier is replayed first
/// via mmap, then the hot tier is replayed on top. When the hot tier grows
/// beyond HOT_TIER_MAX_BYTES, compact() promotes it to the cold tier.
///
/// # File layout on disk
/// ```text
/// my_database.log          <- hot tier (active writes, small)
/// my_database.cold.log     <- cold tier (archived, read-only after promotion)
/// my_database.log.snapshot.bin  <- binary snapshot of the hot tier
/// ```
pub struct TieredStorage {
    /// The hot-tier writer. All new writes go here via AsyncDiskStorage's
    /// MPSC channel + background Tokio task.
    hot: Arc<AsyncDiskStorage>,

    /// Path to the hot log file (e.g. "my_database.log").
    hot_path: String,

    /// Path to the cold log file (e.g. "my_database.cold.log").
    /// This file is append-only from the perspective of TieredStorage —
    /// we only ever append promoted hot-tier data to it.
    cold_path: String,
}

impl TieredStorage {
    /// Create a new TieredStorage.
    ///
    /// `hot_path` is the path to the active log file (e.g. "my_database.log").
    /// The cold path is derived automatically as `<hot_path>.cold.log`
    /// (e.g. "my_database.log.cold.log" → we strip ".log" first for cleanliness,
    /// giving "my_database.cold.log").
    pub fn new(hot_path: &str) -> Result<Self, DbError> {
        // Derive the cold path from the hot path.
        // If hot_path ends in ".log", replace it with ".cold.log".
        // Otherwise, just append ".cold.log".
        let cold_path = if hot_path.ends_with(".log") {
            // "my_database.log" → "my_database.cold.log"
            format!("{}.cold.log", &hot_path[..hot_path.len() - 4])
        } else {
            format!("{}.cold.log", hot_path)
        };

        // Open the hot-tier writer. AsyncDiskStorage creates the file if it
        // doesn't exist and starts the background flush task.
        let hot = Arc::new(AsyncDiskStorage::new(hot_path)?);

        Ok(Self {
            hot,
            hot_path: hot_path.to_string(),
            cold_path,
        })
    }

    /// Return the current size of the hot log file in bytes.
    /// Used by compact() to decide whether to promote to the cold tier.
    fn hot_log_size(&self) -> u64 {
        std::fs::metadata(&self.hot_path)
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Append all entries from the hot log to the cold log file.
    ///
    /// This is called during promotion (when the hot log is too large).
    /// After promotion, the hot log is compacted to a fresh minimal state.
    ///
    /// The cold log is opened in append mode so previous cold data is preserved.
    fn promote_hot_to_cold(&self, hot_entries: &[LogEntry]) -> Result<(), DbError> {
        // Open the cold log in append mode — create it if it doesn't exist.
        // We never truncate the cold log; we only ever add to it.
        let cold_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.cold_path)?;

        // Use a BufWriter for efficient batched writes.
        let mut w = std::io::BufWriter::new(cold_file);

        // Write each hot entry as a JSON line to the cold log.
        for entry in hot_entries {
            writeln!(w, "{}", serde_json::to_string(entry)?)?;
        }

        // Flush to ensure all bytes reach the OS buffer before we return.
        use std::io::Write;
        w.flush()?;

        tracing::info!(
            "🧊 Promoted {} entries from hot tier to cold tier ({})",
            hot_entries.len(),
            self.cold_path
        );

        Ok(())
    }
}

impl StorageBackend for TieredStorage {
    /// Write a new entry to the hot tier.
    ///
    /// This is identical to AsyncDiskStorage::write_entry — the entry is
    /// serialised to JSON and sent over the MPSC channel to the background
    /// writer task. Returns immediately without blocking.
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        // Delegate entirely to the hot-tier writer.
        self.hot.write_entry(entry)
    }

    /// Read all log entries from both tiers into a Vec.
    ///
    /// Cold tier entries come first, then hot tier entries on top.
    /// For the same key, the hot entry wins (it's more recent).
    /// Used by EncryptedStorage which needs the full list to decrypt.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        let mut entries = Vec::new();

        // Read cold tier first (older data).
        if let Some(reader) = MmapLogReader::open(&self.cold_path) {
            reader.stream_entries(0, |e, _| entries.push(e));
        }

        // Read hot tier on top (newer data overwrites cold on replay).
        let hot_entries = self.hot.read_log()?;
        entries.extend(hot_entries);

        Ok(entries)
    }

    /// Compact the database.
    ///
    /// Two cases:
    ///
    /// Case 1 — Hot log is LARGE (> HOT_TIER_MAX_BYTES):
    ///   Promote the current hot entries to the cold tier, then write a minimal
    ///   hot log containing only the entries NOT already in the cold tier.
    ///   This keeps the hot log small for fast future startups.
    ///
    /// Case 2 — Hot log is SMALL:
    ///   Just compact the hot tier in place (same as AsyncDiskStorage::compact).
    ///   No promotion needed — the hot log is already small enough.
    ///
    /// In both cases, a binary snapshot of the hot tier is written so the next
    /// startup can skip replaying the hot log entirely.
    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError> {
        let hot_size = self.hot_log_size();

        if hot_size > HOT_TIER_MAX_BYTES {
            // ── Case 1: Hot tier is large — promote to cold ───────────────────
            tracing::info!(
                "🔥→🧊 Hot tier is {:.1} MB — promoting to cold tier",
                hot_size as f64 / 1_048_576.0
            );

            // Promote all current entries to the cold log.
            // `entries` is the complete current state of the database.
            self.promote_hot_to_cold(&entries)?;

            // After promotion, the cold tier contains everything.
            // The hot tier can now start fresh with an empty log.
            // We write a minimal hot log with zero entries (just the snapshot).
            let seq = count_log_lines(&self.hot_path);
            if let Err(e) = write_snapshot(&self.hot_path, &[], seq) {
                tracing::warn!("⚠️  Failed to write hot snapshot after promotion: {}", e);
            }

            // Write an empty compacted hot log (no entries — all data is in cold).
            let temp_path = format!("{}.tmp", self.hot_path);
            write_compacted_log(&temp_path, &[])?;

            // Signal the background task to swap the hot log file.
            // This is the same sentinel mechanism used by AsyncDiskStorage.
            self.hot.compact(vec![])?;

            tracing::info!("✅ Promotion complete. Hot tier reset to empty.");
        } else {
            // ── Case 2: Hot tier is small — compact in place ──────────────────
            tracing::info!(
                "🗜️  Compacting hot tier in place ({:.1} MB)",
                hot_size as f64 / 1_048_576.0
            );

            // Write a binary snapshot of the current hot state.
            let seq = count_log_lines(&self.hot_path);
            if let Err(e) = write_snapshot(&self.hot_path, &entries, seq) {
                tracing::warn!("⚠️  Failed to write hot snapshot: {}", e);
            }

            // Delegate the actual log rewrite to the hot-tier writer.
            self.hot.compact(entries)?;
        }

        Ok(())
    }

    /// Stream all log entries into state using mmap for the cold tier and
    /// snapshot + streaming for the hot tier.
    ///
    /// Startup sequence:
    ///   1. mmap the cold log → stream entries via OS page cache (zero copy).
    ///   2. Load hot snapshot (if exists) → apply entries from it.
    ///   3. Stream hot delta (lines after the snapshot) → apply remaining entries.
    ///
    /// Hot entries applied in step 2/3 overwrite cold entries from step 1
    /// for the same key, giving the correct final state.
    fn stream_log_into(&self, f: &mut dyn FnMut(LogEntry, u32)) -> Result<u64, DbError> {
        let mut total = 0u64;

        // ── Step 1: Replay cold tier via mmap ────────────────────────────────
        // MmapLogReader::open returns None if the cold log doesn't exist yet
        // (first run, or before the first promotion).
        if let Some(cold_reader) = MmapLogReader::open(&self.cold_path) {
            let cold_line_count = cold_reader.line_count();
            tracing::info!(
                "🧊 Replaying cold tier via mmap ({} lines, file paged by OS)",
                cold_line_count
            );
            // Stream all cold entries — skip_lines=0 means read from the start.
            cold_reader.stream_entries(0, |e, l| {
                f(e, l);
                total += 1;
            });
        }

        // ── Step 2 & 3: Replay hot tier (snapshot + delta) ───────────────────
        // Delegate to AsyncDiskStorage's stream_log_into which handles the
        // snapshot + delta logic for the hot log file.
        let hot_count = self.hot.stream_log_into(f)?;
        total += hot_count;

        tracing::info!(
            "✅ Tiered startup replay complete ({} total entries: cold + {} hot)",
            total,
            hot_count
        );

        Ok(total)
    }

    /// Read exactly `length` bytes from the log at `offset`.
    ///
    /// In TieredStorage, we first check if the offset belongs to the cold log.
    /// If not, we fall back to the hot log.
    fn read_at(&self, offset: u64, length: u32) -> Result<Vec<u8>, DbError> {
        use std::io::{Read, Seek, SeekFrom};
        
        let cold_size = std::fs::metadata(&self.cold_path).map(|m| m.len()).unwrap_or(0);

        if offset < cold_size {
            // Document is in the cold tier.
            let mut file = File::open(&self.cold_path)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut buffer = vec![0u8; length as usize];
            file.read_exact(&mut buffer)?;
            Ok(buffer)
        } else {
            // Document is in the hot tier.
            // Note: The offset passed to this function is cumulative (cold + hot).
            // We must subtract the cold size to get the relative offset in the hot file.
            self.hot.read_at(offset - cold_size, length)
        }
    }
}
