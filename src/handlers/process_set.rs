use serde_json::{Value, json};
use crate::{engine, validation};
use uuid::Uuid;


fn resolve_extends(doc: Value, db: &engine::Db) -> Value {
    // Only objects can have an `extends` block — pass everything else through unchanged.
    let obj = match doc.as_object() {
        Some(o) => o,
        None => return doc,
    };

    // If there is no `extends` key, return the document unchanged.
    // This is the fast path for the vast majority of inserts.
    let extends_map = match obj.get("extends").and_then(|v| v.as_object()) {
        Some(m) => m.clone(),
        None => return doc,
    };

    // Clone the document into a mutable map and remove the `extends` key.
    // The stored document must never contain `extends` — it is a directive,
    // not a data field.
    let mut result = obj.clone();
    result.remove("extends");

    // For each alias → "collection.key" reference, fetch the referenced document
    // and embed it under the alias key.
    for (alias, ref_val) in &extends_map {
        if let Some(ref_str) = ref_val.as_str() {
            // Split "collection.key" on the FIRST dot only.
            // This allows keys that themselves contain dots (e.g. "memory.mem4.v2").
            if let Some(dot_pos) = ref_str.find('.') {
                let ref_collection = &ref_str[..dot_pos];  // e.g. "memory"
                let ref_key        = &ref_str[dot_pos + 1..]; // e.g. "mem4"

                // O(1) hash-map lookup — no scanning, no joins at query time.
                if let Some(referenced_doc) = db.get(ref_collection, ref_key) {
                    // Embed the full referenced document under the alias key.
                    result.insert(alias.clone(), referenced_doc);
                }
                // If the reference is not found, the alias key is simply not added.
                // We never fail the insert because of an unresolvable reference.
            }
        }
    }

    Value::Object(result)
}

/// Handle a SET (insert/upsert) request.
///
/// Accepts two data formats:
///   - Object map: { "collection": "users", "data": { "u1": {...}, "u2": {...} } }
///     Keys are provided by the client. Existing documents are overwritten.
///   - Array:      { "collection": "users", "data": [ {...}, {...} ] }
///     Keys are auto-generated as UUIDv7 strings. Returns the generated IDs.
pub fn process_set(db: &engine::Db, payload: &Value, max_body_size: usize) -> (u16, Value) {
    // Only "collection" and "data" are valid for a set/insert request.
    const SET_ALLOWED: &[&str] = &["collection", "data"];
    if let Err(e) = validation::validate_allowed_properties(payload, SET_ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }
    if let Err(e) = validation::validate_request(payload, max_body_size) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    let col = payload["collection"].as_str().unwrap_or("default");

    match payload.get("data") {
        // ── Object map format ─────────────────────────────────────────────────
        // { "data": { "u1": { "name": "Alice" }, "u2": { "name": "Bob" } } }
        Some(Value::Object(data_map)) => {
            // Collect all key-value pairs into a Vec for batch insert.
            let mut items = Vec::new();
            for (k, v) in data_map {
                let resolved = resolve_extends(v.clone(), db);
                items.push((k.clone(), resolved));
            }

            match db.insert_batch(col, items) {
                Ok(_) => (200, json!({ "status": "ok", "count": data_map.len() })),
                Err(e) => (500, json!({ "error": "Database write failed", "details": e.to_string(), "statusCode": 500 }))
            }
        },

        // ── Array format ──────────────────────────────────────────────────────
        // { "data": [ { "name": "Alice" }, { "name": "Bob" } ] }
        // Auto-generates UUIDv7 keys for each document.
        Some(Value::Array(data_arr)) => {
            let mut items = Vec::new();
            let mut generated_ids = Vec::new();

            for item in data_arr {
                // UUIDv7 is time-ordered — documents inserted together will have
                // adjacent keys, which is good for range scans.
                let id = Uuid::now_v7().to_string();
                generated_ids.push(id.clone());
                // resolve_extends() handles the `extends` block for auto-keyed
                // documents exactly the same as for named-key documents.
                let resolved = resolve_extends(item.clone(), db);
                items.push((id, resolved));
            }

            match db.insert_batch(col, items) {
                Ok(_) => (200, json!({
                    "status": "ok",
                    "count": data_arr.len(),
                    "ids": generated_ids
                })),
                Err(e) => (500, json!({ "error": "Database write failed", "details": e.to_string(), "statusCode": 500 }))
            }
        },

        // Missing or invalid data field.
        _ => (400, json!({ "error": "Missing 'data' (object or array)", "statusCode": 400 }))
    }
}