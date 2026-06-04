use serde_json::{json, Value};

pub enum SetSuccess {
    Inserted(usize),
    InsertedWithIds { count: usize, ids: Vec<String> },
}

impl SetSuccess {
    /// Consumes the success state and formats it into the exact HTTP tuple
    pub fn into_response(&self) -> (u16, Value) {
        match self {
            SetSuccess::Inserted(count) => (200, json!({ "status": "ok", "count": count })),
            SetSuccess::InsertedWithIds { count, ids } => {
                (200, json!({ "status": "ok", "count": count, "ids": ids }))
            }
        }
    }
}
