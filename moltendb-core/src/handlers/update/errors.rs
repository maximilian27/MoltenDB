use crate::handlers::common::errors::OperationError;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum UpdateError {
    UpdateFailed(String),
    ReservedFields,
    VersionConflict,
    MissingDataMap,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::UpdateFailed(details) => write!(f, "Database update failed: {details}"),
            UpdateError::VersionConflict => write!(f, "Conflict: Document version is outdated"),
            UpdateError::ReservedFields => write!(f, "Fields starting with '_' are reserved."),
            UpdateError::MissingDataMap => write!(f, "Missing 'data' map"),
        }
    }
}

impl Error for UpdateError {}

// All you have to do is define the codes.
// The `.into_response()` method is automatically inherited!
impl OperationError for UpdateError {
    fn status_code(&self) -> u16 {
        match self {
            UpdateError::ReservedFields | UpdateError::MissingDataMap => 400,
            UpdateError::VersionConflict => 409,
            UpdateError::UpdateFailed(_) => 500,
        }
    }
}
