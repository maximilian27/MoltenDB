use crate::handlers::common::errors::OperationError;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum StatsError {
    CollectionNotFound,
}

impl fmt::Display for StatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StatsError::CollectionNotFound => write!(f, "Collection not found"),
        }
    }
}

impl Error for StatsError {}

impl OperationError for StatsError {
    fn status_code(&self) -> u16 {
        match self {
            StatsError::CollectionNotFound => 404,
        }
    }
}
