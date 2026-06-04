use serde_json::{json, Value};
use std::error::Error;
use std::fmt;

// ---------------------------------------------------------
// 1. The Global Validation Error
// ---------------------------------------------------------
#[derive(Debug)]
pub struct ValidationError(pub String);

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for ValidationError {}

// ---------------------------------------------------------
// 2. The Shared HTTP Trait
// ---------------------------------------------------------
pub trait HttpError: Error {
    /// Each specific error only needs to define its status code.
    fn status_code(&self) -> u16;

    /// The trait provides this method automatically to anything that implements it!
    fn into_response(&self) -> (u16, Value) {
        let code = self.status_code();
        (
            code,
            json!({ "error": self.to_string(), "statusCode": code }),
        )
    }
}

// 3. Make ValidationError implement our new trait
impl HttpError for ValidationError {
    fn status_code(&self) -> u16 {
        400 // Validation errors are always 400 Bad Request
    }
}
