use super::delete::constants::{DEFAULT_DELETE_COUNT, DELETE_ALLOWED, MAX_DELETE_COUNT};
use super::delete::errors::DeleteError;
use crate::common::payload_fields::PayloadField;
use crate::engine;
use crate::handlers::common::errors::{OperationError, ValidationError};
use crate::handlers::delete::responses::DeleteSuccess;
use crate::query;
use crate::validation;
use serde_json::Value;

/// Handle a DELETE request.
///
/// Five modes:
///   - Single key:    { "collection": "users", "keys": "u1" }
///   - Batch keys:    { "collection": "users", "keys": ["u1", "u2"] }
///   - WHERE filter:  { "collection": "users", "where": { "role": { "$eq": "guest" } } }
///   - Count only:    { "collection": "users", "count": 20 }
///   - Drop all:      { "collection": "users", "drop": true }
///
/// The WHERE mode also accepts `count` (max documents to delete; default 100,
/// max 1000) and `order` ("asc" | "desc"). Matches are ordered by `_seq` before
/// `count` is applied, so a limited delete is deterministic: the default `asc`
/// removes the oldest documents first (lowest `_seq`), `desc` the newest first.
///
/// The count-only mode (no `keys`/`where`/`drop`) removes the oldest (default) or
/// newest `n` documents by `_seq` using the ordered index — `count` is required
/// here (no default) and `order` selects the direction, same as the WHERE mode.
pub fn process_delete(
    db: &engine::Db,
    payload: &Value,
    max_body_size: usize,
    max_keys_per_request: usize,
) -> (u16, Value) {
    if let Err(e) = validation::validate_request(payload, max_body_size, max_keys_per_request) {
        return ValidationError(e.to_string()).into_response();
    }
    // Only "collection", "keys", "where", "count", and "drop" are valid for a delete request.
    if let Err(e) = validation::validate_allowed_properties(payload, DELETE_ALLOWED) {
        return ValidationError(e.to_string()).into_response();
    }

    let col = payload[PayloadField::Collection.as_str()]
        .as_str()
        .unwrap_or("default");

    // Check for drop: true — this removes the entire collection.
    if payload[PayloadField::Drop.as_str()]
        .as_bool()
        .unwrap_or(false)
    {
        return match db.delete_collection(col) {
            Ok(_) => DeleteSuccess::Dropped.into_response(),
            Err(e) => DeleteError::FailedToDropCollection(e.to_string()).into_response(), // Clean, 1-line exit!
        };
    }

    // WHERE-based bulk delete — scan with predicate, delete all matches.
    if let Some(clause) = payload.get(PayloadField::Where.as_str()).cloned() {
        if let Some(n) = payload
            .get(PayloadField::Count.as_str())
            .and_then(|c| c.as_u64())
        {
            if n as usize > MAX_DELETE_COUNT {
                return DeleteError::CountExceeded(MAX_DELETE_COUNT).into_response();
            }
        }
        let count_limit = Some(
            payload
                .get(PayloadField::Count.as_str())
                .and_then(|c| c.as_u64())
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_DELETE_COUNT),
        );
        // `order` decides which documents a count-limited delete removes first.
        // Default is oldest-first (ascending by `_seq`); "desc" removes newest first.
        let default_order_asc = payload
            .get(PayloadField::Order.as_str())
            .and_then(|v| v.as_str())
            .map(|s| s != "desc")
            .unwrap_or(true);
        return match db.delete_filtered(
            col,
            move |_key, doc_bytes| {
                query::evaluate_where_msgpack(doc_bytes, &clause).unwrap_or(false)
            },
            count_limit,
            default_order_asc,
        ) {
            Ok(count) => DeleteSuccess::Deleted(count).into_response(),
            Err(e) => DeleteError::FailedToDelete(e.to_string()).into_response(), // Clean, 1-line exit!
        };
    }

    // Count-only bulk delete — no `keys`/`where`/`drop`, just an explicit `count`.
    // Removes the oldest (default) or newest `n` documents by `_seq` via the
    // ordered index. `count` MUST be explicit (no default) so a tiny payload can
    // never silently destroy a default-sized batch of documents.
    if payload.get(PayloadField::Keys.as_str()).is_none() {
        if let Some(n) = payload
            .get(PayloadField::Count.as_str())
            .and_then(|c| c.as_u64())
        {
            if n as usize > MAX_DELETE_COUNT {
                return DeleteError::CountExceeded(MAX_DELETE_COUNT).into_response();
            }
            // `order` selects direction: default oldest-first (asc), "desc" = newest first.
            let order_asc = payload
                .get(PayloadField::Order.as_str())
                .and_then(|v| v.as_str())
                .map(|s| s != "desc")
                .unwrap_or(true);
            return match db.delete_n(col, n as usize, order_asc) {
                Ok(count) => DeleteSuccess::Deleted(count).into_response(),
                Err(e) => DeleteError::FailedToDelete(e.to_string()).into_response(),
            };
        }
    }

    match payload.get(PayloadField::Keys.as_str()) {
        // Single key delete.
        Some(Value::String(k)) => match db.delete(col, vec![k.to_string()]) {
            Ok(_) => DeleteSuccess::Deleted(1).into_response(),
            Err(e) => DeleteError::FailedToDeleteKey(e.to_string()).into_response(), // Clean, 1-line exit!
        },

        // Batch key delete — collect all keys then delete in one call.
        Some(Value::Array(arr)) => {
            let mut keys = Vec::new();
            for k in arr {
                if let Some(s) = k.as_str() {
                    keys.push(s.to_string());
                }
            }
            let count = keys.len();
            match db.delete(col, keys) {
                Ok(_) => DeleteSuccess::Deleted(count).into_response(),
                Err(e) => DeleteError::FailedToDeleteBatch(e.to_string()).into_response(), // Clean, 1-line exit!
            }
        }
        _ => DeleteError::MissingFields.into_response(),
    }
}
