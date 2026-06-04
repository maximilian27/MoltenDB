use crate::handlers::common::errors::OperationError;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum SnapshotError {
    SnapshotFailed(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::SnapshotFailed(details) => {
                write!(f, "Failed to take snapshot: {details}")
            }
        }
    }
}

impl Error for SnapshotError {}

impl OperationError for SnapshotError {
    fn status_code(&self) -> u16 {
        match self {
            SnapshotError::SnapshotFailed(_) => 500,
        }
    }
}
