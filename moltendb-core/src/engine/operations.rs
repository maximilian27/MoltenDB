// ─── operations.rs ────────────────────────────────────────────────────────────
// This file contains the core CRUD (Create, Read, Update, Delete) operations
// that mutate or query the in-memory database state.
//
// Every function here follows the same pattern:
//   1. Mutate the in-memory DashMap (instant, in RAM).
//   2. Write a LogEntry to the storage backend (persisted to disk/OPFS).
//   3. Broadcast a change event over the Tokio broadcast channel (for WebSocket
//      subscribers who want real-time notifications).
//
// These functions are called by the Db methods in engine/mod.rs, which in turn
// are called by the HTTP handlers in handlers.rs and the WASM worker in worker.rs.
//
// Why separate operations.rs from mod.rs?
//   mod.rs defines the Db struct and its public API. operations.rs contains the
//   actual implementation logic. This keeps mod.rs clean and makes the
//   individual operations easy to find and reason about.
// ─────────────────────────────────────────────────────────────────────────────

// DashMap = concurrent hash map (thread-safe reads and writes without a global lock).
// DashSet = concurrent hash set.
use dashmap::{DashMap, DashSet};
// json! macro creates a serde_json::Value from a JSON literal.
// Value = dynamically-typed JSON value.
use serde_json::{json, Value};
// Standard HashMap — used for return values (not concurrent, just a plain map).
use std::collections::HashMap;
// Arc = thread-safe reference-counted pointer for shared ownership.
use std::sync::Arc;
use tracing::debug;
// Our internal data types.
use super::types::{DbError, LogEntry};
// indexing module — keeps indexes in sync with document mutations.
use super::{indexing, StorageBackend};

/// Returns the current UTC time as an ISO 8601 string (e.g. "2026-03-04T21:58:00Z").
///
/// Used to stamp `_v`, `createdAt`, and `modifiedAt` on every document write.
/// Uses `web-time` for WASM compatibility, `std::time` on native.
fn now_iso() -> String {
    use web_time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (s, m, h) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
    let mut d = secs / 86400; let mut y = 1970u64;
    loop { let dy = if (y%4==0 && y%100!=0)||y%400==0{366}else{365}; if d<dy{break;} d-=dy; y+=1; }
    let lp = (y%4==0&&y%100!=0)||y%400==0;
    let md:[u64;12]=[31,if lp{29}else{28},31,30,31,30,31,31,30,31,30,31];
    let mut mo=1u64; for &x in &md{if d<x{break;} d-=x; mo+=1;}
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",y,mo,d+1,h,m,s)
}

/// Retrieve a single document by its key from a collection.
///
/// Returns `Some(value)` if the document exists, `None` if the collection or
/// key doesn't exist. This is an O(1) hash map lookup.
pub fn get(
    // The full in-memory state: collection name → (key → document value).
    state: &DashMap<String, DashMap<String, Value>>,
    collection: &str,
    key: &str,
) -> Option<Value> {
    // state.get(collection) returns None if the collection doesn't exist.
    // The `?` operator propagates None immediately (short-circuit).
    // .map(|v| v.clone()) clones the value out of the DashMap reference guard.
    state.get(collection)?.get(key).map(|v| v.clone())
}

/// Retrieve all documents in a collection as a HashMap.
///
/// Returns an empty HashMap if the collection doesn't exist.
/// This is O(n) in the number of documents — it copies every document.
pub fn get_all(
    state: &DashMap<String, DashMap<String, Value>>,
    collection: &str,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        // Iterate all key-value pairs in the collection's inner DashMap.
        // entry.key() and entry.value() return references — we clone to own.
        for entry in col.iter() {
            results.insert(entry.key().clone(), entry.value().clone());
        }
    }
    results
}

/// Retrieve a specific set of documents by their keys (batch get).
///
/// Only returns documents that actually exist — missing keys are silently
/// skipped. Returns an empty HashMap if the collection doesn't exist.
pub fn get_batch(
    state: &DashMap<String, DashMap<String, Value>>,
    collection: &str,
    keys: Vec<String>,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        for key in keys {
            // Only insert if the key exists — missing keys are ignored.
            if let Some(val) = col.get(&key) {
                results.insert(key, val.clone());
            }
        }
    }
    results
}

/// Insert or overwrite multiple documents in a single batch operation.
///
/// For each item:
///   1. Insert/overwrite the document in the in-memory DashMap.
///   2. Update all indexes that cover this collection.
///   3. Write an INSERT LogEntry to the storage backend.
///   4. Broadcast an "update" event to WebSocket subscribers.
///
/// If any write to storage fails, the function returns an error immediately.
/// The in-memory state may be partially updated at that point — this is
/// acceptable because the log is the source of truth and the in-memory state
/// is rebuilt from it on the next startup.
pub fn insert_batch(
    state: &DashMap<String, DashMap<String, Value>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    // The storage backend — could be AsyncDiskStorage, SyncDiskStorage,
    // EncryptedStorage, or OpfsStorage depending on configuration.
    storage: &Arc<dyn StorageBackend>,
    // Broadcast channel sender — used to notify WebSocket subscribers of changes.
    // The `_` prefix on the error means we intentionally ignore send failures
    // (it's fine if there are no subscribers).
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    items: Vec<(String, Value)>, // list of (key, document) pairs to insert
) -> Result<(), DbError> {
    // Get or create the collection's inner DashMap.
    // entry().or_insert_with() is atomic — safe to call from multiple threads.
    let col = state
        .entry(collection.to_string())
        .or_insert_with(DashMap::new);

    for (key, mut value) in items {
        // ── Versioning + conflict resolution ─────────────────────────────────
        //
        // Every document automatically carries three engine-managed fields:
        //
        //   _v         — u64 version counter. Starts at 1 on first insert,
        //                incremented on every subsequent write. Never decreases.
        //
        //   createdAt  — ISO 8601 timestamp of the very first insert.
        //                Immutable — never changed after the document is created.
        //
        //   modifiedAt — ISO 8601 timestamp of the most recent write.
        //                Updated on every insert_batch and update() call.
        //
        // Conflict rule (last-write-wins by version):
        //   If the stored document already has _v >= the incoming _v, we skip
        //   the write entirely. This prevents an older offline client from
        //   silently overwriting a newer server-side version when syncing.
        //
        //   If the incoming document has no _v field (raw insert from a legacy
        //   client or requests.http), we always allow the write — no version
        //   means no conflict check.
        let now = now_iso();
        if let Some(existing) = col.get(&key) {
            // Document already exists — check for version conflict.
            let existing_v = existing.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
            let incoming_v = value.get("_v").and_then(|v| v.as_u64());

            if let Some(iv) = incoming_v {
                if iv <= existing_v {
                    // Incoming version is not newer — server wins, skip this key.
                    debug!(
                        "⚡ Conflict skip: {}/{} incoming _v={} <= stored _v={}",
                        collection, key, iv, existing_v
                    );
                    continue;
                }
            }

            // Preserve the original createdAt — it must never change.
            let orig_created = existing
                .get("createdAt")
                .and_then(|v| v.as_str())
                .unwrap_or(&now)
                .to_string();

            // Bump the version counter and refresh modifiedAt.
            let new_v = existing_v + 1;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("_v".to_string(), serde_json::json!(new_v));
                obj.insert("createdAt".to_string(), serde_json::json!(orig_created));
                obj.insert("modifiedAt".to_string(), serde_json::json!(now));
            }
        } else {
            // New document — initialise version and timestamps.
            // If the doc already carries _v (e.g. seeded from server sync),
            // we keep it so the version history is preserved.
            if let Some(obj) = value.as_object_mut() {
                if obj.get("_v").is_none() {
                    obj.insert("_v".to_string(), serde_json::json!(1u64));
                }
                obj.insert("createdAt".to_string(), serde_json::json!(now.clone()));
                obj.insert("modifiedAt".to_string(), serde_json::json!(now));
            }
        }

        // Step 1: Insert/overwrite the document in memory.
        col.insert(key.clone(), value.clone());

        // Step 2: Update indexes. index_doc() finds all indexes for this
        // collection and adds this key to the appropriate index entries.
        indexing::index_doc(indexes, collection, &key, &value);

        // Step 3: Build the LogEntry and persist it.
        // "INSERT" is used for both inserts and overwrites — on replay, the
        // last INSERT for a key wins (later entries overwrite earlier ones).
        let entry = LogEntry {
            cmd: "INSERT".to_string(),
            collection: collection.to_string(),
            key: key.clone(),
            value: value.clone(),
        };
        storage.write_entry(&entry)?;

        // Step 4: Broadcast a lean change event to WebSocket subscribers.
        // send() returns Err if there are no active receivers — we ignore that.
        let new_v = value.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
        let _ = tx.send(
            json!({
                "event": "change",
                "collection": collection,
                "key": key,
                "new_v": new_v
            })
            .to_string(),
        );
    }

    Ok(())
}

/// Partially update (merge) a single document with new field values.
///
/// This is a "patch" operation — only the fields present in `updates` are
/// changed; all other fields in the existing document are preserved.
///
/// Returns `Ok(true)` if the document was found and updated,
/// `Ok(false)` if the document doesn't exist (no-op).
///
/// Example: document { name: "Alice", role: "user" } + update { role: "admin" }
///          → result: { name: "Alice", role: "admin" }
pub fn update(
    state: &DashMap<String, DashMap<String, Value>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    key: &str,
    updates: Value, // the partial update — only these fields will be changed
) -> Result<bool, DbError> {
    if let Some(col) = state.get(collection) {
        // col.get_mut(key) returns a mutable reference guard to the document.
        // While this guard is held, no other thread can write to this key.
        if let Some(mut doc) = col.get_mut(key) {
            // Step 1: Remove the document from indexes BEFORE modifying it,
            // so the old field values are removed from the index entries.
            indexing::unindex_doc(indexes, collection, key, &doc);

            // Step 2: Merge the update fields into the existing document.
            // Only top-level fields are merged — nested objects are replaced,
            // not recursively merged.
            if let Some(update_obj) = updates.as_object() {
                if let Some(doc_obj) = doc.as_object_mut() {
                    for (k, v) in update_obj {
                        // _v and createdAt are managed exclusively by the engine.
                        // Callers cannot set them directly — silently skip if present.
                        if k == "_v" || k == "createdAt" { continue; }
                        doc_obj.insert(k.clone(), v.clone());
                    }
                    // Bump the version counter on every update.
                    let old_v = doc_obj.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
                    doc_obj.insert("_v".to_string(), serde_json::json!(old_v + 1));
                    // Stamp the modification time. createdAt is already in the
                    // document and is intentionally left untouched.
                    doc_obj.insert("modifiedAt".to_string(), serde_json::json!(now_iso()));
                }
            }

            // Step 3: Clone the updated document before dropping the guard.
            // We need the new value for indexing and logging.
            let new_value = doc.clone();

            // Step 4: Re-add the document to indexes with its new field values.
            indexing::index_doc(indexes, collection, key, &new_value);

            // Explicitly drop the mutable guard before writing to storage,
            // to avoid holding the lock longer than necessary.
            drop(doc);

            // Step 5: Write the full updated document as an INSERT entry.
            // Note: we write "INSERT" not "UPDATE" — the log format doesn't
            // have a separate UPDATE command. On replay, this INSERT will
            // overwrite the previous version of the document.
            let entry = LogEntry {
                cmd: "INSERT".to_string(),
                collection: collection.to_string(),
                key: key.to_string(),
                value: new_value.clone(),
            };
            storage.write_entry(&entry)?;

            // Step 6: Broadcast a lean change event to WebSocket subscribers.
            let new_v = new_value.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
            let _ = tx.send(
                json!({
                    "event": "change",
                    "collection": collection,
                    "key": key,
                    "new_v": new_v
                })
                .to_string(),
            );
            return Ok(true); // document was found and updated
        }
    }
    Ok(false) // document not found — no-op
}

/// Delete a single document from a collection.
///
/// If the document doesn't exist, this is a no-op (no error).
/// A DELETE LogEntry is always written to the log, even if the document
/// didn't exist in memory — this ensures the log is consistent.
pub fn delete(
    state: &DashMap<String, DashMap<String, Value>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    key: &str,
) -> Result<(), DbError> {
    if let Some(col) = state.get(collection) {
        // Remove the document from indexes before removing it from state.
        // We need the old value to know which index entries to clean up.
        if let Some(old_val) = col.get(key) {
            indexing::unindex_doc(indexes, collection, key, old_val.value());
        }
        // Remove the document from the in-memory collection.
        col.remove(key);
    }

    // Write a DELETE entry to the log.
    // The `value` field is null for DELETE entries — only collection + key matter.
    let entry = LogEntry {
        cmd: "DELETE".to_string(),
        collection: collection.to_string(),
        key: key.to_string(),
        value: json!(null),
    };
    storage.write_entry(&entry)?;

    // Broadcast a lean delete event to WebSocket subscribers.
    let _ = tx.send(
        json!({
            "event": "change",
            "collection": collection,
            "key": key,
            "new_v": null
        })
        .to_string(),
    );
    Ok(())
}

/// Delete multiple documents from a collection in a single call.
///
/// Each document is removed from indexes and state individually, and a
/// separate DELETE LogEntry is written for each key. If the collection
/// doesn't exist, this is a no-op.
pub fn delete_batch(
    state: &DashMap<String, DashMap<String, Value>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    keys: Vec<String>,
) -> Result<(), DbError> {
    if let Some(col) = state.get(collection) {
        for key in keys {
            // Remove from indexes before removing from state.
            if let Some(old_val) = col.get(&key) {
                indexing::unindex_doc(indexes, collection, &key, old_val.value());
            }

            // Remove the document from the in-memory collection.
            col.remove(&key);

            // Write a DELETE entry for this key.
            let entry = LogEntry {
                cmd: "DELETE".to_string(),
                collection: collection.to_string(),
                key: key.clone(),
                value: json!(null),
            };
            storage.write_entry(&entry)?;

            // Broadcast a lean delete event.
            let event = json!({
                "event": "change",
                "collection": collection,
                "key": key,
                "new_v": null
            })
            .to_string();
            let _ = tx.send(event);
        }
    }

    Ok(())
}

/// Drop an entire collection — removes all documents and its indexes.
///
/// This is an irreversible operation. A DROP LogEntry is written to the log
/// so the collection is not recreated on the next startup.
///
/// After this call:
///   - The collection no longer exists in the in-memory state.
///   - All indexes for this collection are removed.
///   - The DROP entry in the log ensures the collection stays gone on restart.
pub fn delete_collection(
    state: &DashMap<String, DashMap<String, Value>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
) -> Result<(), DbError> {
    // Remove the entire collection from the in-memory state map.
    state.remove(collection);

    // Remove all indexes that belong to this collection.
    // retain() keeps only entries where the closure returns true.
    // We remove any index whose name starts with "collection:" (e.g. "users:role").
    indexes.retain(|k, _| !k.starts_with(&format!("{}:", collection)));

    // Write a DROP entry to the log.
    // The `key` field is "*" as a convention meaning "all keys".
    let entry = LogEntry {
        cmd: "DROP".to_string(),
        collection: collection.to_string(),
        key: "*".to_string(),
        value: json!(null),
    };
    storage.write_entry(&entry)?;

    // Broadcast a lean drop event to WebSocket subscribers.
    let event = json!({
        "event": "change",
        "collection": collection,
        "key": "*",
        "new_v": null
    })
    .to_string();
    let _ = tx.send(event);

    Ok(())
}
