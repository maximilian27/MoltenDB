use serde_json::{Value, json};
use crate::engine;

pub fn process_snapshot(db: &engine::Db) -> (u16, Value) {
    match db.compact() {
        Ok(_) => (200, json!({ "status": "ok", "message": "Snapshot taken successfully" })),
        Err(e) => (500, json!({ "error": "Failed to take snapshot", "details": e.to_string(), "statusCode": 500 }))
    }
}
