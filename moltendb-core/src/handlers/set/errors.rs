use crate::handlers::common::errors::OperationError;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum SetError {
    ReservedFields,
    InvalidTtl,
    InvalidMaxSize,
    InsertConflict,
    StorageFault(String),
    InsertFailed(String),
    MissingDataMap,
    #[cfg(feature = "schema")]
    SchemaValidationError(String),
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetError::ReservedFields => write!(
                f,
                "Fields starting with '_' are reserved for internal use and cannot be set by the client."
            ),
            SetError::InvalidTtl => write!(f, "'ttl' must be a non-negative integer (seconds)"),
            SetError::InvalidMaxSize => write!(f, "'maxSize' must be a positive integer"),
            SetError::InsertConflict => write!(f, "Conflict: Document version is outdated"),
            SetError::StorageFault(details) => {
                write!(f, "Service unavailable -- storage fault: {details}")
            }
            SetError::InsertFailed(details) => write!(f, "Database write failed: {details}"),
            SetError::MissingDataMap => write!(f, "Missing 'data' (object or array)"),
            #[cfg(feature = "schema")]
            SetError::SchemaValidationError(msg) => write!(f, "{msg}"),
        }
    }
}

impl Error for SetError {}

impl OperationError for SetError {
    fn status_code(&self) -> u16 {
        match self {
            SetError::ReservedFields
            | SetError::InvalidTtl
            | SetError::InvalidMaxSize
            | SetError::MissingDataMap => 400,
            #[cfg(feature = "schema")]
            SetError::SchemaValidationError(_) => 400,
            SetError::InsertConflict => 409,
            SetError::StorageFault(_) => 503,
            SetError::InsertFailed(_) => 500,
        }
    }
}
