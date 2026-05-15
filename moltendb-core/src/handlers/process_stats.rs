use serde_json::{Value, json};
use crate::engine;
use crate::engine::ttl;

const STATS_ALLOWED: &[&str] = &["collection"];

pub fn process_stats(db: &engine::Db, payload: &Value) -> (u16, Value) {
    if let Err(e) = crate::validation::validate_allowed_properties(payload, STATS_ALLOWED) {
        return (400, json!({ "error": e.to_string(), "statusCode": 400 }));
    }

    let now = ttl::now_ms();

    if let Some(col) = payload.get("collection").and_then(|v| v.as_str()) {
        // Single collection stats
        match db.collection_count(col) {
            None => (404, json!({ "error": "Collection not found", "statusCode": 404 })),
            Some(count) => {
                if let Some(exp) = db.get_ttl_expiry(col) {
                    if now >= exp {
                        return (200, json!({
                            "collection": col,
                            "count": 0,
                            "expired": true,
                            "expiresAt": ttl::ms_to_iso(exp)
                        }));
                    }
                    (200, json!({
                        "collection": col,
                        "count": count,
                        "expiresAt": ttl::ms_to_iso(exp)
                    }))
                } else {
                    (200, json!({ "collection": col, "count": count }))
                }
            }
        }
    } else {
        // All collections stats
        let counts = db.all_collection_counts();
        let mut collections = serde_json::Map::new();
        let mut total = 0usize;

        for (col, count) in counts {
            if let Some(exp) = db.get_ttl_expiry(&col) {
                if now >= exp {
                    collections.insert(col, json!({
                        "count": 0,
                        "expired": true,
                        "expiresAt": ttl::ms_to_iso(exp)
                    }));
                    continue;
                }
                collections.insert(col.clone(), json!({
                    "count": count,
                    "expiresAt": ttl::ms_to_iso(exp)
                }));
            } else {
                collections.insert(col, json!({ "count": count }));
            }
            total += count;
        }

        (200, json!({ "collections": collections, "total": total }))
    }
}
