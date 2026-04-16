// ─── types.rs ─────────────────────────────────────────────────────────────────
// This file defines the core data types shared across the entire engine:
//
//   • LogEntry — the atomic unit written to the persistent log file.
//                Every database operation (insert, delete, drop, index) is
//                recorded as a LogEntry. The log is the source of truth;
//                the in-memory DashMaps are just a cache of it.
//
//   • DbError  — the unified error type for all database operations.
//                Using a single error enum means callers only need to handle
//                one error type, and we can use the `?` operator everywhere.
// ─────────────────────────────────────────────────────────────────────────────

// serde's Serialize/Deserialize derive macros automatically generate code to
// convert our structs to/from JSON (and bincode for snapshots).
use serde::{Deserialize, Serialize};
// Value is serde_json's dynamically-typed JSON value — used for document data.
use serde_json::Value;
// fmt is used to implement the Display trait (human-readable error messages).
use std::fmt;

/// The atomic unit of data in MoltenDB's append-only log.
///
/// Every mutation to the database is recorded as a LogEntry appended to the
/// log file. On startup, the log is replayed from top to bottom to rebuild
/// the in-memory state. This is the "log-structured" part of MoltenDB.
///
/// The four command types and their meanings:
///
///   "INSERT" — insert or overwrite a document.
///              `collection` = which collection, `key` = document ID,
///              `value` = the full JSON document.
///
///   "DELETE" — delete a single document.
///              `collection` + `key` identify the document; `value` is null.
///
///   "DROP"   — delete an entire collection.
///              `collection` = which collection; `key` and `value` are unused.
///
///   "INDEX"  — record that an index was created on a field.
///              `collection` = which collection, `key` = field name,
///              `value` is null. The index data itself is rebuilt from the
///              INSERT entries during replay.
///
///   "ENC"    — a sentinel used by EncryptedStorage. The real LogEntry is
///              encrypted inside `value` as a base64 string. The engine
///              never sees ENC entries directly — EncryptedStorage decrypts
///              them before returning them from read_log().
#[derive(Serialize, Deserialize)]
pub struct LogEntry {
    /// The command type: "INSERT", "DELETE", "DROP", "INDEX", or "ENC".
    pub cmd: String,
    /// The name of the collection this entry belongs to.
    pub collection: String,
    /// The document key (for INSERT/DELETE) or field name (for INDEX).
    pub key: String,
    /// The document value (for INSERT) or null (for DELETE/DROP/INDEX).
    /// For ENC entries, this holds the base64-encoded ciphertext.
    pub value: Value,
}

/// All possible errors that can occur in the database engine.
///
/// Using an enum instead of a string means callers can pattern-match on the
/// error type and handle each case differently if needed.
#[derive(Debug)]
pub enum DbError {
    /// A file system I/O error (e.g. disk full, permission denied).
    /// Wraps std::io::Error so the original OS error is preserved.
    Io(std::io::Error),

    /// A JSON serialization or deserialization error.
    /// Wraps serde_json::Error so the original parse error is preserved.
    Serialization(serde_json::Error),

    /// A Mutex was poisoned — this happens when a thread panicked while
    /// holding the lock. The lock is now in an undefined state and cannot
    /// be safely acquired. This is a programming error, not a user error.
    LockPoisoned,

    /// A generic write failure used when the specific cause is not an I/O
    /// error — for example, when the MPSC channel is closed (server shutting
    /// down) or when an OPFS browser API call fails (returns a JS error).
    WriteError,
}

/// Implement Display so DbError can be printed with `{}` formatting.
/// This is also required by the `std::error::Error` trait.
impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Delegate to the inner error's Display implementation.
            DbError::Io(err) => write!(f, "Disk I/O Error: {}", err),
            DbError::Serialization(err) => write!(f, "Data Serialization Error: {}", err),
            DbError::LockPoisoned => write!(f, "Internal thread lock was poisoned"),
            DbError::WriteError => write!(f, "Failed to send data to storage backend"),
        }
    }
}

/// Allow `std::io::Error` to be converted to `DbError` with the `?` operator.
/// This means any function returning `Result<_, DbError>` can use `?` on
/// file I/O operations without manually wrapping the error.
impl From<std::io::Error> for DbError {
    fn from(err: std::io::Error) -> Self {
        DbError::Io(err)
    }
}

/// Allow `serde_json::Error` to be converted to `DbError` with the `?` operator.
/// This means any function returning `Result<_, DbError>` can use `?` on
/// JSON serialization/deserialization operations.
impl From<serde_json::Error> for DbError {
    fn from(err: serde_json::Error) -> Self {
        DbError::Serialization(err)
    }
}
