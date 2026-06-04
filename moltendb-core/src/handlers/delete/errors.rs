use serde_json::{json, Value};
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum DeleteError {
    FailedToDropCollection(String),
    FailedToDelete(String),
    FailedToDeleteKey(String),
    FailedToDeleteBatch(String),
    CountExceedsMax(usize),
    MissingFields,
    ValidationError(String),
}

impl fmt::Display for DeleteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeleteError::FailedToDropCollection(details) => {
                write!(f, "Failed to drop collection: {details}")
            }
            DeleteError::FailedToDelete(details) => {
                write!(f, "Failed to delete: {details}")
            }
            DeleteError::FailedToDeleteKey(details) => {
                write!(f, "Failed to delete key: {details}")
            }
            DeleteError::FailedToDeleteBatch(details) => {
                write!(f, "Failed to delete batch: {details}")
            }
            DeleteError::CountExceedsMax(max) => {
                write!(f, "'count' cannot exceed {max}")
            }
            DeleteError::MissingFields => {
                write!(
                    f,
                    "Missing 'keys' (string or array), 'where', or 'drop': true"
                )
            }
            DeleteError::ValidationError(details) => {
                write!(f, "{details}")
            }
        }
    }
}

// 1. Hook into the Rust standard library error ecosystem
impl Error for DeleteError {}

// 2. FFI and JSON Response Helpers
impl DeleteError {
    /// Maps the internal engine error to a standard HTTP status code
    pub fn status_code(&self) -> u16 {
        match self {
            // Client errors
            DeleteError::CountExceedsMax(_)
            | DeleteError::MissingFields
            | DeleteError::ValidationError(_) => 400,

            // Engine/Storage errors
            DeleteError::FailedToDropCollection(_)
            | DeleteError::FailedToDelete(_)
            | DeleteError::FailedToDeleteKey(_)
            | DeleteError::FailedToDeleteBatch(_) => 500,
        }
    }

    /// Consumes the error and formats it directly into the exact tuple
    /// required by the Node/Python/WASM HTTP wrappers.
    pub fn into_response(&self) -> (u16, Value) {
        let code = self.status_code();
        (
            code,
            json!({ "error": self.to_string(), "statusCode": code }),
        )
    }
}
