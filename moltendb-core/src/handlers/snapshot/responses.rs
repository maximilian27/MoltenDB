use serde_json::{json, Value};

pub enum SnapshotSuccess {
    Taken,
}

impl SnapshotSuccess {
    pub fn into_response(self) -> (u16, Value) {
        match self {
            SnapshotSuccess::Taken => (
                200,
                json!({ "status": "ok", "message": "Snapshot taken successfully" }),
            ),
        }
    }
}
