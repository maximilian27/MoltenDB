use crate::handlers::common::errors::OperationError;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum SchemaError {
    MissingCollection,
    MissingSchemaFields,
    InvalidSchema(String),
    InvalidTtl,
    InvalidMaxSize,
    DatabaseError(String),
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaError::MissingCollection => write!(f, "Missing 'collection' name"),
            SchemaError::MissingSchemaFields => {
                write!(
                    f,
                    "At least one of 'schema', 'ttl', or 'maxSize' must be provided"
                )
            }
            SchemaError::InvalidSchema(details) => write!(f, "Invalid Schema: {details}"),
            SchemaError::InvalidTtl => {
                write!(f, "'ttl' must be a non-negative integer (seconds)")
            }
            SchemaError::InvalidMaxSize => write!(f, "'maxSize' must be a positive integer"),
            SchemaError::DatabaseError(details) => write!(f, "Database error: {details}"),
        }
    }
}

impl Error for SchemaError {}

impl OperationError for SchemaError {
    fn status_code(&self) -> u16 {
        match self {
            SchemaError::MissingCollection
            | SchemaError::MissingSchemaFields
            | SchemaError::InvalidSchema(_)
            | SchemaError::InvalidTtl
            | SchemaError::InvalidMaxSize => 400,
            SchemaError::DatabaseError(_) => 500,
        }
    }
}
