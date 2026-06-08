// ��� wasm.rs �����������������������������������������������������������������
// This file implements OpfsStorage � the browser-side StorageBackend that
// persists the database log using the Origin Private File System (OPFS) API.
//
// What is OPFS?
//   OPFS is a browser API that gives web apps access to a private, sandboxed
//   file system. Unlike localStorage or IndexedDB, OPFS provides real file
//   handles with byte-level read/write access. The files are:
//     � Private to the origin (website) � other sites cannot access them.
//     � Persistent across page reloads and browser restarts.
//     � Not visible in the browser's normal file picker.
//     � Accessible only from a Web Worker (not the main thread) via the
//       synchronous FileSystemSyncAccessHandle API.
//
// Why a Web Worker?
//   The synchronous OPFS API (FileSystemSyncAccessHandle) blocks the calling
//   thread while reading/writing. This is fine in a Web Worker (which runs on
//   a separate thread), but would freeze the UI if called on the main thread.
//   MoltenDB always runs its database in a Web Worker for this reason.
//
// Log format:
//   Same as the native disk backend � one JSON-encoded LogEntry per line.
//   The file is read in full on startup and replayed into the in-memory state.
//
// Compaction:
//   The OPFS file is truncated to 0 bytes and rewritten with only the current
//   state (no temp file swap needed � OPFS handles are exclusive to the worker).
// �����������������������������������������������������������������������������

// Only compile this file when targeting WebAssembly.
#![cfg(target_arch = "wasm32")]

// The StorageBackend trait that OpfsStorage implements.
use super::StorageBackend;
// Our internal data types.
use crate::engine::types::{DbError, LogEntry};
// Mutex gives us exclusive access to the file handle across async boundaries.
// In WASM, "threads" are Web Workers � Mutex prevents two workers from writing
// to the same file simultaneously (though in practice we only have one worker).
use std::ops::ControlFlow;
use std::sync::Mutex;
// wasm_bindgen bridges Rust and JavaScript � it generates the JS glue code
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
    /// and getSize() � all synchronous (blocking) operations safe to call from
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
        // Get the WorkerGlobalScope � this confirms we're running in a Web Worker.
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
        // unchecked_into() skips the runtime type check � we trust the browser API.
        let root_dir: web_sys::FileSystemDirectoryHandle = root_val.unchecked_into();

        // Step 2: Get (or create) a file handle for our database file.
        // FileSystemGetFileOptions with create:true means "create if not exists".
        let opts = web_sys::FileSystemGetFileOptions::new();
        opts.set_create(true);

        let file_val: JsValue =
            JsFuture::from(root_dir.get_file_handle_with_options(db_name, &opts))
                .await
                .map_err(|_| DbError::WriteError)?;
        let file_handle: web_sys::FileSystemFileHandle = file_val.unchecked_into();

        // Step 3: Open a synchronous access handle on the file.
        // This gives us a FileSystemSyncAccessHandle with blocking read/write.
        // Only one sync handle can be open per file at a time � if another
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
            // We ignore the result � there's nothing useful we can do if it fails
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
        handle
            .truncate_with_f64(0.0)
            .map_err(|_| DbError::WriteError)?;
        handle.flush().map_err(|_| DbError::WriteError)?;
        // Release the exclusive lock so the JS side can removeEntry().
        handle.close();
        Ok(())
    }
}

/// Implement the StorageBackend trait for OPFS-based storage.
impl StorageBackend for OpfsStorage {
    fn stream_log_into(
        &self,
        f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>,
    ) -> Result<u64, DbError> {
        let size = {
            let handle = self.handle.lock().expect("db handle mutex poisoned");
            handle.get_size().map_err(|_| DbError::WriteError)? as usize
        };
        if size == 0 {
            return Ok(0);
        }

        let chunk_size = 64 * 1024; // 64KB
        let mut offset = 0;
        let mut count = 0u64;
        let mut remaining = Vec::new();

        while offset < size {
            let to_read = (size - offset).min(chunk_size);
            let mut chunk = vec![0u8; to_read];
            {
                let handle = self.handle.lock().expect("db handle mutex poisoned");
                let opts = web_sys::FileSystemReadWriteOptions::new();
                opts.set_at(offset as f64);
                handle
                    .read_with_u8_array_and_options(&mut chunk, &opts)
                    .map_err(|_| DbError::WriteError)?;
            }
            offset += to_read;
            remaining.extend_from_slice(&chunk);

            while remaining.len() >= 4 {
                let mut len_bytes = [0u8; 4];
                len_bytes.copy_from_slice(&remaining[0..4]);
                let msg_len = u32::from_le_bytes(len_bytes) as usize;

                if remaining.len() < 4 + msg_len {
                    // Need more data for this message
                    break;
                }

                let entry_data = &remaining[4..4 + msg_len];
                if let Some(entry) =
                    crate::common::system_field_tokens::log_entry_from_msgpack(entry_data)
                {
                    if let ControlFlow::Break(_) = f(entry, msg_len as u32) {
                        return Ok(count); // Break out completely
                    }
                    count += 1;
                }

                // Remove the processed message from the buffer
                remaining.drain(0..4 + msg_len);
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
        // Serialize the entry to MessagePack and prefix with length.
        let mut encoded = crate::common::system_field_tokens::log_entry_to_msgpack(entry)
            .map_err(|_| DbError::WriteError)?;
        let mut bytes = (encoded.len() as u32).to_le_bytes().to_vec();
        bytes.append(&mut encoded);

        // Acquire the Mutex to get exclusive access to the file handle.
        let handle = self.handle.lock().expect("db handle mutex poisoned");

        // Get the current file size � this is where we'll write (append).
        // get_size() returns the file size in bytes as a float (JS number).
        let size = handle.get_size().map_err(|_| DbError::WriteError)? as f64;

        // Set the write position to the end of the file (append mode).
        let opts = web_sys::FileSystemReadWriteOptions::new();
        opts.set_at(size);

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
        let mut entries = Vec::new();
        self.stream_log_into(&mut |entry, _| {
            entries.push(entry);
            ControlFlow::Continue(())
        })?;
        Ok(entries)
    }

    /// Truncate the OPFS file to 0 bytes and close the sync handle.
    /// Delegates to `truncate_and_close()` so the JS side can then call
    /// `removeEntry()` on the OPFS directory without hitting a "file locked" error.
    fn clear_opfs(&self) -> Result<(), DbError> {
        self.truncate_and_close()
    }

    #[cfg(not(feature = "schema"))]
    fn compact_from_maps(
        &self,
        state: &dashmap::DashMap<std::sync::Arc<str>, dashmap::DashMap<String, Box<[u8]>>>,
    ) -> Result<(), DbError> {
        self.do_compact(state)
    }

    #[cfg(feature = "schema")]
    fn compact_from_maps(
        &self,
        state: &dashmap::DashMap<std::sync::Arc<str>, dashmap::DashMap<String, Box<[u8]>>>,
        schemas: &dashmap::DashMap<
            String,
            std::sync::Arc<(serde_json::Value, jsonschema::Validator)>,
        >,
    ) -> Result<(), DbError> {
        self.do_compact(state, Some(schemas))
    }
}

impl OpfsStorage {
    /// Compact the OPFS file by truncating it and rewriting only current state.
    /// Uses chunked writes to avoid memory spikes (OOM).
    #[cfg(not(feature = "schema"))]
    fn do_compact(
        &self,
        state: &dashmap::DashMap<std::sync::Arc<str>, dashmap::DashMap<String, Box<[u8]>>>,
    ) -> Result<(), DbError> {
        let handle = self.handle.lock().expect("db handle mutex poisoned");

        // Truncate the file to 0 bytes � erases existing content.
        handle
            .truncate_with_f64(0.0)
            .map_err(|_| DbError::WriteError)?;

        let mut offset = 0.0;
        let mut buffer = Vec::new();
        let chunk_size = 64 * 1024; // 64KB

        let mut write_buffer = |buf: &mut Vec<u8>| -> Result<(), DbError> {
            if buf.is_empty() {
                return Ok(());
            }
            let opts = web_sys::FileSystemReadWriteOptions::new();
            opts.set_at(offset);
            handle
                .write_with_u8_array_and_options(buf, &opts)
                .map_err(|_| DbError::WriteError)?;
            offset += buf.len() as f64;
            buf.clear();
            Ok(())
        };

        // Write documents
        for col_ref in state.iter() {
            let col_name = col_ref.key().clone();
            for item_ref in col_ref.value().iter() {
                if let Some(value) =
                    crate::common::system_field_tokens::msgpack_to_value(item_ref.value())
                {
                    let entry = LogEntry::new(
                        crate::common::log_commands::LogCommand::IKEY_INSERT.to_string(),
                        col_name.to_string(),
                        item_ref.key().clone(),
                        value,
                    );
                    if let Ok(mut encoded) =
                        crate::common::system_field_tokens::log_entry_to_msgpack(&entry)
                    {
                        buffer.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
                        buffer.append(&mut encoded);

                        if buffer.len() >= chunk_size {
                            write_buffer(&mut buffer)?;
                        }
                    }
                }
            }
        }

        // Flush remaining buffer
        write_buffer(&mut buffer)?;

        handle.flush().map_err(|_| DbError::WriteError)?;
        Ok(())
    }

    #[cfg(feature = "schema")]
    fn do_compact(
        &self,
        state: &dashmap::DashMap<std::sync::Arc<str>, dashmap::DashMap<String, Box<[u8]>>>,
        schemas: Option<
            &dashmap::DashMap<String, std::sync::Arc<(serde_json::Value, jsonschema::Validator)>>,
        >,
    ) -> Result<(), DbError> {
        let handle = self.handle.lock().expect("db handle mutex poisoned");

        // Truncate the file to 0 bytes � erases existing content.
        handle
            .truncate_with_f64(0.0)
            .map_err(|_| DbError::WriteError)?;

        let mut offset = 0.0;
        let mut buffer = Vec::new();
        let chunk_size = 64 * 1024; // 64KB

        let mut write_buffer = |buf: &mut Vec<u8>| -> Result<(), DbError> {
            if buf.is_empty() {
                return Ok(());
            }
            let opts = web_sys::FileSystemReadWriteOptions::new();
            opts.set_at(offset);
            handle
                .write_with_u8_array_and_options(buf, &opts)
                .map_err(|_| DbError::WriteError)?;
            offset += buf.len() as f64;
            buf.clear();
            Ok(())
        };

        // Write documents
        for col_ref in state.iter() {
            let col_name = col_ref.key().clone();
            for item_ref in col_ref.value().iter() {
                if let Some(value) =
                    crate::common::system_field_tokens::msgpack_to_value(item_ref.value())
                {
                    let entry = LogEntry::new(
                        crate::common::log_commands::LogCommand::IKEY_INSERT.to_string(),
                        col_name.to_string(),
                        item_ref.key().clone(),
                        value,
                    );
                    if let Ok(mut encoded) =
                        crate::common::system_field_tokens::log_entry_to_msgpack(&entry)
                    {
                        buffer.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
                        buffer.append(&mut encoded);

                        if buffer.len() >= chunk_size {
                            write_buffer(&mut buffer)?;
                        }
                    }
                }
            }
        }

        // Write schemas if provided
        if let Some(schemas_map) = schemas {
            for schema_ref in schemas_map.iter() {
                let schema_val = schema_ref.value();
                let schema_json = &schema_val.0;
                let entry = LogEntry::new(
                    crate::common::log_commands::LogCommand::IKEY_SCHEMA.to_string(),
                    schema_ref.key().to_string(),
                    "".to_string(),
                    schema_json.clone(),
                );
                if let Ok(mut encoded) =
                    crate::common::system_field_tokens::log_entry_to_msgpack(&entry)
                {
                    buffer.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
                    buffer.append(&mut encoded);

                    if buffer.len() >= chunk_size {
                        write_buffer(&mut buffer)?;
                    }
                }
            }
        }

        // Flush remaining buffer
        write_buffer(&mut buffer)?;

        handle.flush().map_err(|_| DbError::WriteError)?;
        Ok(())
    }
}
