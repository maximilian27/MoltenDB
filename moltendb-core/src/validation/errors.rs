#[derive(Debug)]
pub enum ValidationError {
    InvalidCollectionName(String),
    InvalidKeyName(String),
    InvalidFieldName(String),
    CollectionNameTooLong,
    KeyNameTooLong,
    PayloadTooLarge,
    InvalidJsonDepth,
    TooManyKeys,
    UnknownProperty(String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidCollectionName(n) => write!(
                f,
                "Invalid collection name: '{}'. Must be alphanumeric with _, - only (1-64 chars)",
                n
            ),
            ValidationError::InvalidKeyName(n) => write!(
                f,
                "Invalid key name: '{}'. Must be alphanumeric with _, -, . only (1-256 chars)",
                n
            ),
            ValidationError::InvalidFieldName(n) => write!(
                f,
                "Invalid field name: '{}'. Must be alphanumeric with _, -, . only (1-128 chars)",
                n
            ),
            ValidationError::CollectionNameTooLong => {
                write!(f, "Collection name too long (max 64 characters)")
            }
            ValidationError::KeyNameTooLong => write!(f, "Key name too long (max 256 characters)"),
            ValidationError::PayloadTooLarge => write!(f, "Payload too large (max 10MB)"),
            ValidationError::InvalidJsonDepth => write!(f, "JSON nesting too deep (max 32 levels)"),
            ValidationError::TooManyKeys => write!(f, "Too many keys in single request (max 1000)"),
            ValidationError::UnknownProperty(n) => write!(
                f,
                "Unknown property: '{}'. Check the API docs for supported properties",
                n
            ),
        }
    }
}

impl std::error::Error for ValidationError {}
