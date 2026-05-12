// ─── operations/insert.rs ─────────────────────────────────────────────────────
// Insert operation: insert.
// ─────────────────────────────────────────────────────────────────────────────

use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::Arc;
use super::common::now_iso;
use super::super::StorageBackend;
use super::super::types::{DbError, LogEntry};

/// Parameters for the [`insert`] operation.
///
/// Grouping these into a struct keeps the function signature within Clippy's
/// argument-count limit and makes call sites more readable.
pub struct InsertParams<'a> {
    pub state: &'a DashMap<Arc<str>, DashMap<String, Box<[u8]>>>,
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
        storage,
        tx,
        #[cfg(feature = "schema")]
        schemas,
        collection,
        items,
    } = params;
    let col = state
        .entry(Arc::from(collection))
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
        
        // Decode existing MsgPack bytes → Value for versioning check.
        let existing_val: Option<Value> = col.get(&key).and_then(|b| rmp_serde::from_slice::<Value>(&b).ok());

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

        // Step 1: Insert/overwrite in memory as MsgPack-encoded bytes.
        if let Ok(bytes) = rmp_serde::to_vec(&value) {
            col.insert(key.clone(), bytes.into_boxed_slice());
        }

        // Step 2: Persist within the transaction.
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
