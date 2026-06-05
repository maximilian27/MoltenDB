use serde_json::Value;

pub enum GetSuccess {
    /// A single document returned directly (no array wrapper).
    Document(Value),
    /// An array of documents.
    Documents(Vec<Value>),
}

impl GetSuccess {
    /// Consumes the success state and formats it into the exact HTTP tuple.
    pub fn into_response(self) -> (u16, Value) {
        match self {
            GetSuccess::Document(doc) => (200, doc),
            GetSuccess::Documents(arr) => (200, Value::Array(arr)),
        }
    }
}
