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
///
/// In the hybrid Bitcask model, if the document is "Cold", it is fetched from
/// storage, deserialized, and returned.
pub fn get(
    // The full in-memory state: collection name → (key → document state).
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &Arc<dyn StorageBackend>,
    collection: &str,
    key: &str,
) -> Option<Value> {
    let col = state.get(collection)?;
    let doc_state = col.get(key)?;
    
    match doc_state.value() {
        crate::engine::types::DocumentState::Hot(v) => Some(v.clone()),
        crate::engine::types::DocumentState::Cold(ptr) => {
            // Fetch from disk.
            if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length) {
                // The bytes in the log are a full LogEntry JSON.
                if let Ok(entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                    return Some(entry.value);
                }
            }
            None
        }
    }
}

/// Retrieve all documents in a collection as a HashMap.
///
/// Returns an empty HashMap if the collection doesn't exist.
/// This is O(n) in the number of documents — it copies every document.
pub fn get_all(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &Arc<dyn StorageBackend>,
    collection: &str,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        for entry in col.iter() {
            let key = entry.key();
            match entry.value() {
                crate::engine::types::DocumentState::Hot(v) => {
                    results.insert(key.clone(), v.clone());
                }
                crate::engine::types::DocumentState::Cold(ptr) => {
                    if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length) {
                        if let Ok(log_entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                            results.insert(key.clone(), log_entry.value);
                        }
                    }
                }
            }
        }
    }
    results
}

/// Retrieve a specific set of documents by their keys (batch get).
///
/// Only returns documents that actually exist — missing keys are silently
/// skipped. Returns an empty HashMap if the collection doesn't exist.
pub fn get_batch(
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    storage: &Arc<dyn StorageBackend>,
    collection: &str,
    keys: Vec<String>,
) -> HashMap<String, Value> {
    let mut results = HashMap::new();
    if let Some(col) = state.get(collection) {
        for key in keys {
            if let Some(entry) = col.get(&key) {
                match entry.value() {
                    crate::engine::types::DocumentState::Hot(v) => {
                        results.insert(key, v.clone());
                    }
                    crate::engine::types::DocumentState::Cold(ptr) => {
                        if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length) {
                            if let Ok(log_entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                                results.insert(key, log_entry.value);
                            }
                        }
                    }
                }
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
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    #[cfg(feature = "schema")] schemas: &DashMap<String, Arc<(Value, jsonschema::Validator)>>,
    collection: &str,
    items: Vec<(String, Value)>,
) -> Result<(), DbError> {
    let col = state
        .entry(collection.to_string())
        .or_insert_with(DashMap::new);

    // TX_BEGIN: Start a transaction.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    for (key, mut value) in items {
        let now = now_iso();
        
        // We need to check the existing document for versioning.
        // If it's Cold, we MUST fetch it to check _v and createdAt.
        let mut existing_val = None;
        if let Some(doc_state) = col.get(&key) {
            match doc_state.value() {
                crate::engine::types::DocumentState::Hot(v) => {
                    existing_val = Some(v.clone());
                }
                crate::engine::types::DocumentState::Cold(ptr) => {
                    if let Ok(bytes) = storage.read_at(ptr.offset, ptr.length) {
                        if let Ok(entry) = serde_json::from_slice::<crate::engine::types::LogEntry>(&bytes) {
                            existing_val = Some(entry.value);
                        }
                    }
                }
            }
        }

        if let Some(existing) = existing_val {
            // ... (existing logic) ...
            let existing_v = existing.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
            let incoming_v = value.get("_v").and_then(|v| v.as_u64());

            if let Some(iv) = incoming_v {
                if iv <= existing_v {
                    debug!("⚡ Conflict error: {}/{} incoming _v={} <= stored _v={}", collection, key, iv, existing_v);
                    return Err(DbError::Conflict);
                }
            }

            let orig_created = existing.get("createdAt").and_then(|v| v.as_str()).unwrap_or(&now).to_string();
            let new_v = existing_v + 1;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("_v".to_string(), serde_json::json!(new_v));
                obj.insert("createdAt".to_string(), serde_json::json!(orig_created));
                obj.insert("modifiedAt".to_string(), serde_json::json!(now));
            }

            // Schema Validation: Check the document BEFORE index update and WAL write.
            #[cfg(feature = "schema")]
            crate::engine::schema::validate_document(schemas, collection, &value)?;

            // Unindex the OLD value before overwriting.
            indexing::unindex_doc(indexes, collection, &key, &existing);
        } else {
            if let Some(obj) = value.as_object_mut() {
                if obj.get("_v").is_none() {
                    obj.insert("_v".to_string(), serde_json::json!(1u64));
                }
                obj.insert("createdAt".to_string(), serde_json::json!(now.clone()));
                obj.insert("modifiedAt".to_string(), serde_json::json!(now));
            }

            // Schema Validation: Check the document BEFORE index update and WAL write.
            #[cfg(feature = "schema")]
            crate::engine::schema::validate_document(schemas, collection, &value)?;
        }

        // Step 1: Insert/overwrite in memory (always Hot for new writes).
        col.insert(key.clone(), crate::engine::types::DocumentState::Hot(value.clone()));

        // Step 2: Update indexes.
        indexing::index_doc(indexes, collection, &key, &value);

        // Step 3: Persist within the transaction.
        let entry = LogEntry::new(
            "INSERT".to_string(),
            collection.to_string(),
            key.clone(),
            value.clone(),
        );
        storage.write_entry(&entry)?;

        // Step 4: Broadcast a lean change event to WebSocket subscribers.
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

    // TX_COMMIT: Successfully complete the transaction.
    storage.write_entry(&LogEntry::new(
        "TX_COMMIT".into(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

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
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    #[cfg(feature = "schema")] schemas: &DashMap<String, Arc<(Value, jsonschema::Validator)>>,
    collection: &str,
    key: &str,
    updates: Value, // the partial update — only these fields will be changed
) -> Result<bool, DbError> {
    if let Some(col) = state.get(collection) {
        if let Some(doc) = {
            if let Some(doc_state) = col.get(key) {
                // Fetch the full document value first.
                Some(match doc_state.value() {
                    crate::engine::types::DocumentState::Hot(v) => v.clone(),
                    crate::engine::types::DocumentState::Cold(ptr) => {
                        let bytes = storage.read_at(ptr.offset, ptr.length)?;
                        let entry: crate::engine::types::LogEntry = serde_json::from_slice(&bytes)?;
                        entry.value
                    }
                })
            } else {
                None
            }
        } {
            let mut doc = doc;

            // Step 1: Remove the document from indexes BEFORE modifying it,
            // so the old field values are removed from the index entries.
            indexing::unindex_doc(indexes, collection, key, &doc);

            // Step 2: Merge the update fields into the existing document.
            // Only top-level fields are merged — nested objects are replaced,
            // not recursively merged.
            if let Some(update_obj) = updates.as_object() {
                // If the caller provides a "_v" field in the update, it acts as a guard.
                // If the current version is not equal to this guard, we return Conflict.
                let existing_v = doc.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(guard_v) = update_obj.get("_v").and_then(|v| v.as_u64()) {
                    if guard_v != existing_v {
                        debug!("⚡ Conflict error: {}/{} update guard _v={} != stored _v={}", collection, key, guard_v, existing_v);
                        return Err(DbError::Conflict);
                    }
                }

                if let Some(doc_obj) = doc.as_object_mut() {
                    for (k, v) in update_obj {
                        // _v and createdAt are managed exclusively by the engine.
                        // Callers cannot set them directly — silently skip if present.
                        if k == "_v" || k == "createdAt" { continue; }
                        doc_obj.insert(k.clone(), v.clone());
                    }
                    // Bump the version counter on every update.
                    doc_obj.insert("_v".to_string(), serde_json::json!(existing_v + 1));
                    // Stamp the modification time. createdAt is already in the
                    // document and is intentionally left untouched.
                    doc_obj.insert("modifiedAt".to_string(), serde_json::json!(now_iso()));
                }
            }

            // Step 3: Clone the updated document and validate against schema.
            let new_value = doc.clone();
            #[cfg(feature = "schema")]
            crate::engine::schema::validate_document(schemas, collection, &new_value)?;

            // Step 4: Re-add the document to indexes with its new field values.
            indexing::index_doc(indexes, collection, key, &new_value);

            // Step 5: Update state (now Hot).
            col.insert(key.to_string(), crate::engine::types::DocumentState::Hot(new_value.clone()));

            // Step 6: Write the full updated document as an INSERT entry.
            let entry = LogEntry::new(
                "INSERT".to_string(),
                collection.to_string(),
                key.to_string(),
                new_value.clone(),
            );
            storage.write_entry(&entry)?;

            // Step 7: Broadcast a lean change event to WebSocket subscribers.
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
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    key: &str,
) -> Result<(), DbError> {
    // TX_BEGIN: Start a transaction for the delete.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    if let Some(col) = state.get(collection) {
        if let Some(val) = {
            if let Some(doc_state) = col.get(key) {
                Some(match doc_state.value() {
                    crate::engine::types::DocumentState::Hot(v) => v.clone(),
                    crate::engine::types::DocumentState::Cold(ptr) => {
                        let bytes = storage.read_at(ptr.offset, ptr.length)?;
                        let entry: crate::engine::types::LogEntry = serde_json::from_slice(&bytes)?;
                        entry.value
                    }
                })
            } else {
                None
            }
        } {
            indexing::unindex_doc(indexes, collection, key, &val);
        }
        // Remove the document from the in-memory collection.
        col.remove(key);
    }

    // Write a DELETE entry to the log.
    let entry = LogEntry::new(
        "DELETE".to_string(),
        collection.to_string(),
        key.to_string(),
        json!(null),
    );
    storage.write_entry(&entry)?;

    // TX_COMMIT: Successfully complete the transaction.
    storage.write_entry(&LogEntry::new(
        "TX_COMMIT".into(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

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
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
    keys: Vec<String>,
) -> Result<(), DbError> {
    // TX_BEGIN: Start a transaction for the batch delete.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    if let Some(col) = state.get(collection) {
        for key in keys {
            // Remove from indexes before removing from state.
            if let Some(val) = {
                if let Some(doc_state) = col.get(&key) {
                    Some(match doc_state.value() {
                        crate::engine::types::DocumentState::Hot(v) => v.clone(),
                        crate::engine::types::DocumentState::Cold(ptr) => {
                            let bytes = storage.read_at(ptr.offset, ptr.length)?;
                            let entry: crate::engine::types::LogEntry = serde_json::from_slice(&bytes)?;
                            entry.value
                        }
                    })
                } else {
                    None
                }
            } {
                indexing::unindex_doc(indexes, collection, &key, &val);
            }

            // Remove the document from the in-memory collection.
            col.remove(&key);

            // Write a DELETE entry for this key.
            let entry = LogEntry::new(
                "DELETE".to_string(),
                collection.to_string(),
                key.clone(),
                json!(null),
            );
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

    // TX_COMMIT: Successfully complete the transaction.
    storage.write_entry(&LogEntry::new(
        "TX_COMMIT".into(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

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
    state: &DashMap<String, DashMap<String, crate::engine::types::DocumentState>>,
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    tx: &tokio::sync::broadcast::Sender<String>,
    collection: &str,
) -> Result<(), DbError> {
    // Step 1: Remove from memory.
    state.remove(collection);
    // Step 2: Remove all indexes for this collection.
    indexes.retain(|k, _| !k.starts_with(&format!("{}:", collection)));

    // Step 3: Persist the DROP command.
    let entry = LogEntry::new(
        "DROP".to_string(),
        collection.to_string(),
        "*".to_string(),
        json!(null),
    );
    storage.write_entry(&entry)?;

    // Step 4: Broadcast a lean drop event.
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
