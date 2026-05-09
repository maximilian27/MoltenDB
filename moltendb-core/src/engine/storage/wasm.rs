// ─── wasm.rs ─────────────────────────────────────────────────────────────────
// This file implements OpfsStorage — the browser-side StorageBackend that
// persists the database log using the Origin Private File System (OPFS) API.
//
// What is OPFS?
//   OPFS is a browser API that gives web apps access to a private, sandboxed
//   file system. Unlike localStorage or IndexedDB, OPFS provides real file
//   handles with byte-level read/write access. The files are:
//     • Private to the origin (website) — other sites cannot access them.
//     • Persistent across page reloads and browser restarts.
//     • Not visible in the browser's normal file picker.
//     • Accessible only from a Web Worker (not the main thread) via the
//       synchronous FileSystemSyncAccessHandle API.
//
// Why a Web Worker?
//   The synchronous OPFS API (FileSystemSyncAccessHandle) blocks the calling
//   thread while reading/writing. This is fine in a Web Worker (which runs on
//   a separate thread), but would freeze the UI if called on the main thread.
//   MoltenDB always runs its database in a Web Worker for this reason.
//
// Log format:
//   Same as the native disk backend — one JSON-encoded LogEntry per line.
//   The file is read in full on startup and replayed into the in-memory state.
//
// Compaction:
//   The OPFS file is truncated to 0 bytes and rewritten with only the current
//   state (no temp file swap needed — OPFS handles are exclusive to the worker).
// ─────────────────────────────────────────────────────────────────────────────

// Only compile this file when targeting WebAssembly.
#![cfg(target_arch = "wasm32")]

// The StorageBackend trait that OpfsStorage implements.
use super::StorageBackend;
// Our internal data types.
use crate::engine::types::{DbError, LogEntry};
// Mutex gives us exclusive access to the file handle across async boundaries.
// In WASM, "threads" are Web Workers — Mutex prevents two workers from writing
// to the same file simultaneously (though in practice we only have one worker).
use std::sync::Mutex;
use std::ops::ControlFlow;
// wasm_bindgen bridges Rust and JavaScript — it generates the JS glue code
// that lets Rust call browser APIs and vice versa.
use wasm_bindgen::prelude::*;
// JsCast provides the unchecked_into() method for casting JS values to
// specific types (like casting a JsValue to a FileSystemDirectoryHandle).
use wasm_bindgen::JsCast;
// JsFuture converts a JavaScript Promise into a Rust Future so we can
// await it with `.await` in async Rust code.
use wasm_bindgen_futures::JsFuture;

/// Browser-side storage backend using the Origin Private File System (OPFS).
///
/// Holds a synchronous file handle that allows byte-level read/write access
/// to a file in the browser's private storage. The handle is wrapped in a
/// Mutex so it can be safely shared across async boundaries.
pub struct OpfsStorage {
    /// The synchronous OPFS file handle.
    /// FileSystemSyncAccessHandle provides read(), write(), flush(), truncate(),
    /// and getSize() — all synchronous (blocking) operations safe to call from
    /// a Web Worker.
    handle: Mutex<web_sys::FileSystemSyncAccessHandle>,
    /// If true, call flush() after every write.
    sync_mode: bool,
}

impl OpfsStorage {
    /// Open (or create) an OPFS file with the given `db_name` and return an
    /// OpfsStorage wrapping its sync access handle.
    ///
    /// This is async because the OPFS directory/file APIs return Promises.
    /// The sequence is:
    ///   1. Get the OPFS root directory from navigator.storage.getDirectory()
    ///   2. Get (or create) a file handle for `db_name`
    ///   3. Open a synchronous access handle on that file
    pub async fn new(db_name: &str, sync_mode: bool) -> Result<Self, DbError> {
        // Get the WorkerGlobalScope — this confirms we're running in a Web Worker.
        // If we're on the main thread, dyn_into() fails and we return WriteError.
        let global = js_sys::global()
            .dyn_into::<web_sys::WorkerGlobalScope>()
            .map_err(|_| DbError::WriteError)?;

        // Access the StorageManager via navigator.storage.
        // This is the entry point to the OPFS API.
        let navigator: web_sys::WorkerNavigator = global.navigator();
        let storage = navigator.storage();

        // Step 1: Get the OPFS root directory.
        // storage.get_directory() returns a Promise<FileSystemDirectoryHandle>.
        // JsFuture::from() wraps it as a Rust Future, and .await resolves it.
        let root_val: JsValue = JsFuture::from(storage.get_directory())
            .await
            .map_err(|_| DbError::WriteError)?;
        // Cast the resolved JsValue to the concrete FileSystemDirectoryHandle type.
        // unchecked_into() skips the runtime type check — we trust the browser API.
        let root_dir: web_sys::FileSystemDirectoryHandle = root_val.unchecked_into();

        // Step 2: Get (or create) a file handle for our database file.
        // FileSystemGetFileOptions with create:true means "create if not exists".
        let opts = web_sys::FileSystemGetFileOptions::new();
        opts.set_create(true);

        let file_val: JsValue = JsFuture::from(
            root_dir.get_file_handle_with_options(db_name, &opts)
        )
        .await
        .map_err(|_| DbError::WriteError)?;
        let file_handle: web_sys::FileSystemFileHandle = file_val.unchecked_into();

        // Step 3: Open a synchronous access handle on the file.
        // This gives us a FileSystemSyncAccessHandle with blocking read/write.
        // Only one sync handle can be open per file at a time — if another
        // worker already has it open, this will fail (hence the Drop impl below
        // which closes the handle when OpfsStorage is dropped).
        let sync_val: JsValue = JsFuture::from(file_handle.create_sync_access_handle())
            .await
            .map_err(|_| DbError::WriteError)?;
        let sync_handle: web_sys::FileSystemSyncAccessHandle = sync_val.unchecked_into();

        Ok(Self {
            handle: Mutex::new(sync_handle),
            sync_mode,
        })
    }
}

/// Ensure the OPFS sync handle is always closed when OpfsStorage is dropped.
///
/// This is critical: if the handle is not closed (e.g. the worker crashes or
/// is terminated), the file remains locked and the next page load will fail
/// to open a new handle. The Drop impl guarantees cleanup even on panic.
impl Drop for OpfsStorage {
    fn drop(&mut self) {
        // Try to acquire the Mutex. If it's poisoned (a panic occurred while
        // holding it), we still try to close the handle.
        if let Ok(handle) = self.handle.lock() {
            // close() releases the exclusive lock on the OPFS file.
            // We ignore the result — there's nothing useful we can do if it fails
            // during cleanup.
            let _ = handle.close();
        }
    }
}

impl OpfsStorage {
    /// Truncate the OPFS file to 0 bytes and close the sync access handle.
    ///
    /// Called when the user wants to wipe all persisted data. After this call
    /// the handle is closed and this storage instance must not be used again.
    /// The JS side can then call `navigator.storage.getDirectory()` +
    /// `removeEntry()` to delete the OPFS directory, or simply reload the page
    /// so a fresh handle is opened on the now-empty file.
    fn truncate_and_close(&self) -> Result<(), DbError> {
        let handle = self.handle.lock().expect("db handle mutex poisoned");
        // Erase all content.
        handle.truncate_with_f64(0.0).map_err(|_| DbError::WriteError)?;
        handle.flush().map_err(|_| DbError::WriteError)?;
        // Release the exclusive lock so the JS side can removeEntry().
        handle.close();
        Ok(())
    }
}

/// Implement the StorageBackend trait for OPFS-based storage.
impl StorageBackend for OpfsStorage {
    fn stream_log_into(&self, f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>) -> Result<u64, DbError> {
        // Read all file data while holding the mutex, then release the lock
        // before invoking the callback. This prevents a recursive mutex deadlock:
        // the callback (apply_entry) would acquire the same mutex, causing a panic.
        let data = {
            let handle = self.handle.lock().expect("db handle mutex poisoned");

            // Get the file size to know how many bytes to read.
            let size = handle.get_size().map_err(|_| DbError::WriteError)? as usize;

            // If the file is empty (first run), return early before allocating.
            if size == 0 { return Ok(0); }

            // Allocate a buffer exactly the size of the file.
            let mut buf = vec![0u8; size];

            // Set the read position to the beginning of the file.
            let opts = web_sys::FileSystemReadWriteOptions::new();
            opts.set_at(0.0);

            // Read the entire file into the buffer in one call.
            handle
                .read_with_u8_array_and_options(&mut buf, &opts)
                .map_err(|_| DbError::WriteError)?;

            buf
            // Mutex guard is dropped here — lock released before calling f().
        };

        // Convert bytes to a string. from_utf8_lossy replaces invalid UTF-8
        // sequences with the replacement character instead of returning an error.
        let data_str = String::from_utf8_lossy(&data);

        let mut count = 0u64;
        for line in data_str.lines() {
            let length = line.len() as u32;
            if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
                if let ControlFlow::Break(_) = f(entry, length) {
                    break;
                }
                count += 1;
            }
        }
        Ok(count)
    }

    /// Append a single log entry to the OPFS file.
    ///
    /// The entry is serialized to JSON, a newline is appended, and the bytes
    /// are written at the current end of the file (append semantics).
    /// flush() is called after every write to ensure the data is durable.
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        // Serialize the entry to a JSON string and append a newline.
        let mut json_line = serde_json::to_string(entry)?;
        json_line.push('\n');

        // Acquire the Mutex to get exclusive access to the file handle.
        let handle = self.handle.lock().expect("db handle mutex poisoned");

        // Get the current file size — this is where we'll write (append).
        // get_size() returns the file size in bytes as a float (JS number).
        let size = handle.get_size().map_err(|_| DbError::WriteError)? as f64;

        // Set the write position to the end of the file (append mode).
        let opts = web_sys::FileSystemReadWriteOptions::new();
        opts.set_at(size);

        // Convert the string to bytes and write them.
        let mut bytes = json_line.into_bytes();
        handle
            .write_with_u8_array_and_options(&mut bytes, &opts)
            .map_err(|_| DbError::WriteError)?;

        // Flush to ensure the data is persisted to the OPFS file.
        // Without flush(), the data might only be in an OS buffer.
        if self.sync_mode {
            handle.flush().map_err(|_| DbError::WriteError)?;
        }
        Ok(())
    }


    /// Read the entire OPFS file and parse all log entries.
    ///
    /// The whole file is read into a byte buffer, converted to a string,
    /// and split by newlines. Each line is parsed as a JSON LogEntry.
    /// Lines that fail to parse are silently skipped.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        let handle = self.handle.lock().expect("db handle mutex poisoned");

        // Get the file size to know how many bytes to read.
        let size = handle.get_size().map_err(|_| DbError::WriteError)? as usize;

        // If the file is empty (first run), return an empty Vec immediately.
        if size == 0 { return Ok(Vec::new()); }

        // Allocate a buffer exactly the size of the file.
        let mut buf = vec![0u8; size];

        // Set the read position to the beginning of the file.
        let opts = web_sys::FileSystemReadWriteOptions::new();
        opts.set_at(0.0);

        // Read the entire file into the buffer in one call.
        handle
            .read_with_u8_array_and_options(&mut buf, &opts)
            .map_err(|_| DbError::WriteError)?;

        // Convert bytes to a string. from_utf8_lossy replaces invalid UTF-8
        // sequences with the replacement character instead of returning an error.
        let data_str = String::from_utf8_lossy(&buf);

        // Split by newlines, parse each line as JSON, skip failures.
        Ok(data_str.lines()
            .filter_map(|line| serde_json::from_str::<LogEntry>(line).ok())
            .collect())
    }

    /// Truncate the OPFS file to 0 bytes and close the sync handle.
    /// Delegates to `truncate_and_close()` so the JS side can then call
    /// `removeEntry()` on the OPFS directory without hitting a "file locked" error.
    fn clear_opfs(&self) -> Result<(), DbError> {
        self.truncate_and_close()
    }

    /// Return the current size of the OPFS file in bytes.
    ///
    /// Called by the JS worker after every INSERT batch to decide whether to
    /// trigger size-based auto-compaction. Uses `FileSystemSyncAccessHandle.getSize()`
    /// which is a cheap metadata call — it does not read any file content.
    fn get_size(&self) -> Result<u64, DbError> {
        let handle = self.handle.lock().expect("db handle mutex poisoned");
        // get_size() returns the file byte length as a JS number (f64).
        // We cast to u64 — OPFS files are never negative or fractional.
        let size = handle.get_size().map_err(|_| DbError::WriteError)? as u64;
        Ok(size)
    }

    /// Compact the OPFS file by truncating it and rewriting only current state.
    ///
    /// Unlike the native disk backend, we don't need a temp file + rename here
    /// because the OPFS handle is exclusive to this worker — no other process
    /// can be reading or writing the file concurrently.
    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError> {
        let handle = self.handle.lock().expect("db handle mutex poisoned");

        // Truncate the file to 0 bytes — this erases all existing content.
        // truncate_with_f64(0.0) sets the file size to 0.
        handle.truncate_with_f64(0.0).map_err(|_| DbError::WriteError)?;

        // Build the new file content: one JSON line per entry.
        let mut all_data = String::new();
        for entry in entries {
            all_data.push_str(&serde_json::to_string(&entry)?);
            all_data.push('\n');
        }

        // Write all the compacted entries starting at byte 0.
        let mut bytes = all_data.into_bytes();
        let opts = web_sys::FileSystemReadWriteOptions::new();
        opts.set_at(0.0);

        handle
            .write_with_u8_array_and_options(&mut bytes, &opts)
            .map_err(|_| DbError::WriteError)?;

        // Flush to ensure the compacted data is persisted.
        handle.flush().map_err(|_| DbError::WriteError)?;
        Ok(())
    }
}
