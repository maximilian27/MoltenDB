use serde_json::{json, Value};

pub enum DeleteSuccess {
    Dropped,
    Deleted(usize),
}

impl DeleteSuccess {
    /// Consumes the success state and formats it into the exact HTTP tuple
    pub fn into_response(&self) -> (u16, Value) {
        match self {
            DeleteSuccess::Dropped => (200, json!({ "status": "ok", "dropped": true })),
            DeleteSuccess::Deleted(count) => (200, json!({ "status": "ok", "deleted": count })),
        }
    }
}
