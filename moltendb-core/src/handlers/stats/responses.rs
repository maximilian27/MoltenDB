use serde_json::{json, Value};

pub enum StatsSuccess {
    Collection(Value),
    AllCollections {
        collections: serde_json::Map<String, Value>,
        total: usize,
    },
}

impl StatsSuccess {
    pub fn into_response(self) -> (u16, Value) {
        match self {
            StatsSuccess::Collection(data) => (200, data),
            StatsSuccess::AllCollections { collections, total } => {
                (200, json!({ "collections": collections, "total": total }))
            }
        }
    }
}
