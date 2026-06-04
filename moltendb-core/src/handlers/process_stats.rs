use serde_json::{Value, json};
use crate::engine;
use crate::engine::ttl;
use crate::handlers::stats::constants::STATS_ALLOWED;

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
                let max_size = db.max_sizes.get(col).map(|v| *v);
                let expiry   = db.get_ttl_expiry(col);

                if let Some(exp) = expiry {
                    if now >= exp {
                        let mut resp = json!({
                            "collection": col,
                            "count": 0,
                            "expired": true,
                            "expiresAt": ttl::ms_to_iso(exp)
                        });
                        if let Some(max) = max_size {
                            resp["maxSize"] = json!(max);
                        }
                        return (200, resp);
                    }
                    let mut resp = json!({
                        "collection": col,
                        "count": count,
                        "expiresAt": ttl::ms_to_iso(exp)
                    });
                    if let Some(max) = max_size {
                        resp["maxSize"] = json!(max);
                    }
                    (200, resp)
                } else {
                    let mut resp = json!({ "collection": col, "count": count });
                    if let Some(max) = max_size {
                        resp["maxSize"] = json!(max);
                    }
                    (200, resp)
                }
            }
        }
    } else {
        // All collections stats
        let counts = db.all_collection_counts();
        let mut collections = serde_json::Map::new();
        let mut total = 0usize;

        for (col, count) in counts {
            let max_size = db.max_sizes.get(&col).map(|v| *v);
            if let Some(exp) = db.get_ttl_expiry(&col) {
                if now >= exp {
                    let mut entry = json!({
                        "count": 0,
                        "expired": true,
                        "expiresAt": ttl::ms_to_iso(exp)
                    });
                    if let Some(max) = max_size {
                        entry["maxSize"] = json!(max);
                    }
                    collections.insert(col, entry);
                    continue;
                }
                let mut entry = json!({ "count": count, "expiresAt": ttl::ms_to_iso(exp) });
                if let Some(max) = max_size {
                    entry["maxSize"] = json!(max);
                }
                collections.insert(col.clone(), entry);
            } else {
                let mut entry = json!({ "count": count });
                if let Some(max) = max_size {
                    entry["maxSize"] = json!(max);
                }
                collections.insert(col, entry);
            }
            total += count;
        }

        (200, json!({ "collections": collections, "total": total }))
    }
}
