// ─── operations/insert.rs ─────────────────────────────────────────────────────
// Insert operation: insert.
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::{DashMap, DashSet};
use serde_json::{json, Value};
use std::sync::Arc;
use super::common::now_iso;
use super::super::{indexing, StorageBackend};
use super::super::types::{DbError, LogEntry};

/// Parameters for the [`insert`] operation.
///
/// Grouping these into a struct keeps the function signature within Clippy's
/// argument-count limit and makes call sites more readable.
pub struct InsertParams<'a> {
    pub state: &'a DashMap<String, DashMap<String, serde_json::Value>>,
    pub indexes: &'a DashMap<String, DashMap<String, DashSet<String>>>,
    pub storage: &'a Arc<dyn StorageBackend>,
    pub tx: &'a tokio::sync::broadcast::Sender<String>,
    #[cfg(feature = "schema")]
    pub schemas: &'a DashMap<String, Arc<(Value, jsonschema::Validator)>>,
    pub collection: &'a str,
    pub items: Vec<(String, Value)>,
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
pub fn insert(params: InsertParams<'_>) -> Result<(), DbError> {
    let InsertParams {
        state,
        indexes,
        storage,
        tx,
        #[cfg(feature = "schema")]
        schemas,
        collection,
        items,
    } = params;
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
        let existing_val = col.get(&key).map(|v| v.clone());

        if let Some(existing) = existing_val {
            // ... (existing logic) ...
            let existing_v = existing.get("_v").and_then(|v| v.as_u64()).unwrap_or(0);
            let incoming_v = value.get("_v").and_then(|v| v.as_u64());

            if let Some(iv) = incoming_v
                && iv <= existing_v {
                    tracing::debug!("⚡ Conflict error: {}/{} incoming _v={} <= stored _v={}", collection, key, iv, existing_v);
                    return Err(DbError::Conflict);
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
        } else if let Some(obj) = value.as_object_mut() {
            if obj.get("_v").is_none() {
                obj.insert("_v".to_string(), serde_json::json!(1u64));
            }
            obj.insert("createdAt".to_string(), serde_json::json!(now.clone()));
            obj.insert("modifiedAt".to_string(), serde_json::json!(now));

            // Schema Validation: Check the document BEFORE index update and WAL write.
            #[cfg(feature = "schema")]
            crate::engine::schema::validate_document(schemas, collection, &value)?;
        }

        // Step 1: Insert/overwrite in memory.
        col.insert(key.clone(), value.clone());

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
