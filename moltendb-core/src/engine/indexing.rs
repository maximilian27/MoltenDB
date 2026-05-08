// ─── indexing.rs ──────────────────────────────────────────────────────────────
// This file implements MoltenDB's automatic indexing system.
//
// What is an index?
//   An index is a data structure that maps a field value to the set of document
//   keys that have that value. For example, an index on "role" might look like:
//     "admin"  → { "u1", "u4" }
//     "user"   → { "u2", "u3", "u5" }
//   With this index, a query WHERE role = "admin" can find matching documents
//   in O(1) instead of scanning every document in the collection (O(n)).
//
// Index storage structure:
//   indexes: DashMap<String, DashMap<String, DashSet<String>>>
//            │                │                └── set of document keys
//            │                └── field value (e.g. "admin")
//            └── index name: "collection:field" (e.g. "users:role")
//
// Auto-indexing (query heatmap):
//   MoltenDB tracks how many times each field is used in a WHERE clause via
//   a "query heatmap" (DashMap<String, u32>). When a field is queried 3 or
//   more times, an index is automatically created on it. This means you never
//   need to manually create indexes — the engine learns which fields are hot
//   and indexes them automatically.
//
// Index persistence:
//   When an index is created, an "INDEX" LogEntry is written to the log file.
//   On startup, when the log is replayed, INDEX entries register empty index
//   slots. The index data itself is rebuilt by replaying the INSERT entries
//   that follow (index_doc is called for each INSERT during replay).
// ─────────────────────────────────────────────────────────────────────────────

// DashMap = concurrent hash map (thread-safe, no global lock).
// DashSet = concurrent hash set.
use dashmap::{DashMap, DashSet};
use tracing::debug;
// Value = dynamically-typed JSON value.
use serde_json::Value;
// Arc = thread-safe reference-counted pointer.
use std::sync::Arc;
// Our internal types.
use super::types::{DbError, LogEntry};
// StorageBackend trait — needed to write the INDEX entry to the log.
use super::StorageBackend;

/// Update all indexes when a document is inserted or modified.
///
/// For each index that covers `collection`, this function extracts the indexed
/// field's value from the document and adds `key` to the corresponding index
/// entry. If the field doesn't exist in the document, the index is not updated
/// (the document simply won't appear in queries on that field).
///
/// Called from:
///   - operations::insert() — when a new document is written
///   - storage::apply_entry() / replay_log_entries() — during startup replay
pub fn index_doc(
    // The full index map: "collection:field" → (field_value → set of doc keys)
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    collection: &str,
    key: &str,       // the document's primary key (e.g. "u1")
    value: &Value,   // the full document JSON
) {
    // Iterate all existing indexes to find ones that belong to this collection.
    for index_ref in indexes.iter() {
        let index_name = index_ref.key(); // e.g. "users:role"

        // Only process indexes for this collection (prefix match).
        if index_name.starts_with(&format!("{}:", collection)) {
            // Extract the field name from the index name (part after the colon).
            // e.g. "users:role" → "role", "users:meta.logins" → "meta.logins"
            let field_name = index_name.split(':').nth(1).unwrap_or("");
            if field_name.is_empty() { continue; }

            // Use dot-notation to extract the field value from the document.
            // e.g. field "meta.logins" on { meta: { logins: 10 } } → 10
            if let Some(val) = crate::query::get_nested_value(value, &field_name.split('.').collect::<Vec<_>>()) {
                // Convert the value to a string for use as the index key.
                // e.g. the number 10 becomes the string "10".
                let val_str = val.to_string();

                // Add this document key to the set for this field value.
                // entry().or_insert_with() atomically creates the DashSet if it
                // doesn't exist yet, then inserts the key into it.
                index_ref.value()
                    .entry(val_str)
                    .or_insert_with(DashSet::new)
                    .insert(key.to_string());
            }
        }
    }
}

/// Remove a document from all indexes when it is deleted or overwritten.
///
/// This is the inverse of index_doc(). It must be called BEFORE the document
/// is removed from the state map, because we need the old value to know which
/// index entries to remove.
///
/// Called from:
///   - operations::insert() — before overwriting an existing document
///   - operations::delete() / delete_batch() — before deleting a document
///   - storage::apply_entry() / replay_log_entries() — during startup replay
pub fn unindex_doc(
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    collection: &str,
    key: &str,     // the document's primary key
    value: &Value, // the OLD document value (before deletion/overwrite)
) {
    for index_ref in indexes.iter() {
        let index_name = index_ref.key();

        // Only process indexes for this collection.
        if index_name.starts_with(&format!("{}:", collection)) {
            let field_name = index_name.split(':').nth(1).unwrap();

            // Extract the field value from the OLD document.
            if let Some(val) = crate::query::get_nested_value(value, &field_name.split('.').collect::<Vec<_>>()) {
                let val_str = val.to_string();

                // Remove this document key from the index entry for this value.
                // get_mut() returns a mutable reference to the DashSet.
                if let Some(key_set) = index_ref.value().get_mut(&val_str) {
                    key_set.remove(key);
                    // Note: we don't remove the empty DashSet from the index map.
                    // This is a minor memory leak but avoids a race condition where
                    // another thread is simultaneously inserting into the same set.
                }
            }
        }
    }
}

/// Auto-create an index on first query of a field.
///
/// Called from handlers::process_get() for every field in the WHERE clause.
/// If an index already exists for this field, this is a no-op.
pub fn track_query(
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    collection: &str,
    field: &str,
    // The storage backend — needed to persist the INDEX entry if we create one.
    storage: &Arc<dyn StorageBackend>,
    // The full in-memory state — needed to build the index from existing documents.
    state: &DashMap<String, DashMap<String, Value>>,
) -> Result<(), DbError> {
    debug!("🔥 Auto-indexing {}.{}", collection, field);
    create_index(indexes, storage, state, collection, field)
}

/// Create a new index on `collection.field` and persist it to the log.
///
/// This function:
///   1. Scans all existing documents in the collection to build the initial
///      index data (so queries immediately benefit from the index).
///   2. Inserts the index into the in-memory indexes map.
///   3. Writes an "INDEX" LogEntry to the log so the index is recreated on
///      the next startup (the index data is rebuilt from INSERT entries).
///
/// If an index already exists for this field, this is a no-op.
pub fn create_index(
    indexes: &DashMap<String, DashMap<String, DashSet<String>>>,
    storage: &Arc<dyn StorageBackend>,
    state: &DashMap<String, DashMap<String, Value>>,
    collection: &str,
    field: &str,
) -> Result<(), DbError> {
    let index_key = format!("{}:{}", collection, field);

    // Guard against creating duplicate indexes.
    if indexes.contains_key(&index_key) {
        return Ok(());
    }

    // TX_BEGIN: Start a transaction for the index creation.
    let tx_id = uuid::Uuid::new_v4().to_string();
    storage.write_entry(&LogEntry::new(
        "TX_BEGIN".into(),
        collection.into(),
        tx_id.clone(),
        Value::Null,
    ))?;

    // Build the initial index by scanning all existing documents.
    // This is O(n) in the number of documents, but only happens once per field.
    let field_index = DashMap::new();
    if let Some(col) = state.get(collection) {
        for entry in col.iter() {
            // Extract the field value using dot-notation (supports nested fields).
            if let Some(val) = crate::query::get_nested_value(
                entry.value(),
                &field.split('.').collect::<Vec<_>>(),
            ) {
                // Add this document key to the index entry for this field value.
                field_index
                    .entry(val.to_string())
                    .or_insert_with(DashSet::new)
                    .insert(entry.key().clone());
            }
        }
    }

    // Insert the populated index into the in-memory indexes map.
    indexes.insert(index_key, field_index);

    // Write an INDEX entry to the log so this index is recreated on next startup.
    // The `key` field of the LogEntry holds the field name.
    let entry = LogEntry::new(
        "INDEX".to_string(),
        collection.to_string(),
        field.to_string(),
        serde_json::json!(null),
    );

    // Persist the INDEX entry via the storage backend.
    storage.write_entry(&entry)?;

    // TX_COMMIT: Successfully complete the transaction.
    storage.write_entry(&LogEntry::new(
        "TX_COMMIT".into(),
        collection.into(),
        tx_id,
        Value::Null,
    ))?;

    debug!("⚡ Persistent Index created on {}.{}", collection, field);
    Ok(())
}
