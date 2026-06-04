use crate::engine;
use crate::handlers::common::errors::OperationError;
use crate::handlers::snapshot::errors::SnapshotError;
use crate::handlers::snapshot::responses::SnapshotSuccess;
use serde_json::Value;

pub fn process_snapshot(db: &engine::Db) -> (u16, Value) {
    match db.compact() {
        Ok(_) => SnapshotSuccess::Taken.into_response(),
        Err(e) => SnapshotError::SnapshotFailed(e.to_string()).into_response(),
    }
}
