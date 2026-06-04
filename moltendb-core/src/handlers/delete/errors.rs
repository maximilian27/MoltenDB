use crate::handlers::common::errors::OperationError;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum DeleteError {
    FailedToDropCollection(String),
    FailedToDelete(String),
    FailedToDeleteKey(String),
    FailedToDeleteBatch(String),
    CountExceeded(usize),
    MissingFields,
    // ValidationError(String) is GONE!
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
            DeleteError::CountExceeded(max) => {
                write!(f, "'count' cannot exceed {max}")
            }
            DeleteError::MissingFields => {
                write!(
                    f,
                    "Missing 'keys' (string or array), 'where', or 'drop': true"
                )
            }
        }
    }
}

// 1. Hook into the Rust standard library error ecosystem
impl Error for DeleteError {}

// 2. Implement your custom HTTP Trait
impl OperationError for DeleteError {
    fn status_code(&self) -> u16 {
        match self {
            // Client errors
            DeleteError::CountExceeded(_) | DeleteError::MissingFields => 400,

            // Engine/Storage errors
            DeleteError::FailedToDropCollection(_)
            | DeleteError::FailedToDelete(_)
            | DeleteError::FailedToDeleteKey(_)
            | DeleteError::FailedToDeleteBatch(_) => 500,
        }
    }
}
