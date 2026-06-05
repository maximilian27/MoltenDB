use serde_json::{json, Value};

pub enum UpdateSuccess {
    Updated(usize),
}

impl UpdateSuccess {
    /// Consumes the success state and formats it into the exact HTTP tuple
    pub fn into_response(&self) -> (u16, Value) {
        match self {
            UpdateSuccess::Updated(count) => (200, json!({ "status": "ok", "updated": count })),
        }
    }
}
