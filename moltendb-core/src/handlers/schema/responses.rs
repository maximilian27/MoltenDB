use serde_json::{json, Value};

pub enum SchemaSuccess {
    Updated(String),
}

impl SchemaSuccess {
    pub fn into_response(self) -> (u16, Value) {
        match self {
            SchemaSuccess::Updated(col) => (200, json!({ "status": "ok", "collection": col })),
        }
    }
}
