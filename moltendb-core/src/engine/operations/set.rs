// ─── operations/set ─────────────────────────────────────────────────────
// Insert operation: insert.
// ─────────────────────────────────────────────────────────────────────────────

use super::super::types::{DbError, LogEntry};
use super::common::{now_unix_ms, InsertParams};
use crate::common::system_fields::SystemFields;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;


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
        seq_counters,
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
        let now_ms = now_unix_ms();

        // Assign a monotonic sequence number for this document.
        // New docs get a fresh seq; overwrites preserve the existing seq.
        let seq = {
            let counter = seq_counters
                .entry(collection.to_string())
                .or_insert_with(|| AtomicU64::new(0));
            counter.fetch_add(1, Ordering::Relaxed)
        };

        // Decode existing MsgPack bytes → Value for versioning check.
        let existing_val: Option<Value> = col
            .get(&key)
            .and_then(|b| rmp_serde::from_slice::<Value>(&b).ok());

        if let Some(existing) = existing_val {
            let existing_v = existing
                .get(SystemFields::VERSION)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // Preserve original _createdAt; support compact (_ca) and legacy (_createdAt) field names.
            let orig_created: Value = existing
                .get(SystemFields::STORE_CREATED_AT)
                .or_else(|| existing.get("_createdAt"))
                .and_then(|v| v.as_u64())
                .map(|ms| serde_json::json!(ms))
                .unwrap_or_else(|| serde_json::json!(now_ms));
            // Preserve the original _seq so overwritten docs keep their insertion order.
            let orig_seq = existing
                .get(SystemFields::STORE_SEQ)
                .or_else(|| existing.get("_seq"))
                .and_then(|v| v.as_u64())
                .unwrap_or(seq);
            let new_v = existing_v + 1;
            if let Some(obj) = value.as_object_mut() {
                obj.insert(SystemFields::VERSION.to_string(), serde_json::json!(new_v));
                obj.insert(SystemFields::STORE_CREATED_AT.to_string(), orig_created);
                obj.insert(
                    SystemFields::STORE_MODIFIED_AT.to_string(),
                    serde_json::json!(now_ms),
                );
                obj.insert(
                    SystemFields::STORE_SEQ.to_string(),
                    serde_json::json!(orig_seq),
                );
            }

            // Schema Validation: Check the document BEFORE index update and WAL write.
            #[cfg(feature = "schema")]
            crate::engine::schema::validate_document(schemas, collection, &value)?;
        } else if let Some(obj) = value.as_object_mut() {
            obj.insert(SystemFields::VERSION.to_string(), serde_json::json!(1u64));
            obj.insert(
                SystemFields::STORE_CREATED_AT.to_string(),
                serde_json::json!(now_ms),
            );
            obj.insert(
                SystemFields::STORE_MODIFIED_AT.to_string(),
                serde_json::json!(now_ms),
            );
            obj.insert(SystemFields::STORE_SEQ.to_string(), serde_json::json!(seq));

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
        let new_v = value
            .get(SystemFields::VERSION)
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let expires_at_ms = value
            .get(SystemFields::EXPIRES_AT)
            .or_else(|| value.get(SystemFields::STORE_EXPIRES_AT))
            .and_then(|v| v.as_u64());
        let mut event = json!({
            "event": "change",
            "collection": collection,
            "key": key,
            "new_v": new_v
        });
        if let Some(exp) = expires_at_ms {
            event["expires_at_ms"] = json!(exp);
        }
        let _ = tx.send(event.to_string());
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
