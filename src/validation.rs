// ─── validation.rs ────────────────────────────────────────────────────────────
// This file implements input validation for all incoming HTTP requests.
//
// Why validate?
//   Without validation, a malicious client could send:
//     - Collection names like "../etc/passwd" (path traversal)
//     - Deeply nested JSON like { a: { b: { c: ... } } } (stack overflow)
//     - Payloads of hundreds of megabytes (memory exhaustion)
//     - Reserved names like "admin" or "system" (privilege escalation)
//     - Thousands of keys in one request (CPU exhaustion)
//
// All validation happens in validate_request(), which is called at the top
// of every process_* function in handlers.rs before any database work is done.
//
// Validation rules:
//   Collection names: 1–64 chars, alphanumeric + _ and - only, not reserved.
//   Key names:        1–256 chars, alphanumeric + _ - . only.
//   Field names:      1–128 chars, alphanumeric + _ - . only (dot = nested path).
//   JSON depth:       max 32 levels of nesting.
//   Payload size:     max 10 MB.
//   Batch size:       max 1000 keys per request.
// ─────────────────────────────────────────────────────────────────────────────

// Regex = compiled regular expression for pattern matching.
use regex::Regex;
use serde_json::Value;
// LazyLock = initialise a static value lazily on first access (thread-safe).
// Used here to compile regexes once at startup instead of on every request.
use std::sync::LazyLock;

// ─── Compiled regexes ─────────────────────────────────────────────────────────
// Regexes are expensive to compile — we compile them once and reuse them.
// LazyLock ensures the regex is compiled on the first call and cached forever.

/// Valid collection names: 1–64 alphanumeric characters, underscores, or hyphens.
/// Rejects path separators (/ \), dots, spaces, and special characters.
static COLLECTION_NAME_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9_-]{1,64}$").unwrap()
});

/// Valid document keys: 1–256 alphanumeric characters, underscores, hyphens, or dots.
/// Dots are allowed in keys (e.g. "user.123") but not in collection names.
static KEY_NAME_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9_.-]{1,256}$").unwrap()
});

/// Valid field names: 1–128 alphanumeric characters, underscores, hyphens, or dots.
/// Dots are used for nested field access (e.g. "meta.logins").
static FIELD_NAME_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9_.-]{1,128}$").unwrap()
});

// ─── ValidationError enum ─────────────────────────────────────────────────────

/// All possible validation failures. Each variant carries enough context to
/// produce a helpful error message for the client.
#[derive(Debug)]
pub enum ValidationError {
    /// The collection name contains invalid characters or is a reserved name.
    InvalidCollectionName(String),
    /// The document key contains invalid characters or is too long.
    InvalidKeyName(String),
    /// A field name in a projection, WHERE clause, or join contains invalid characters.
    InvalidFieldName(String),
    /// The collection name exceeds 64 characters.
    CollectionNameTooLong,
    /// A document key exceeds 256 characters.
    KeyNameTooLong,
    /// The entire request payload exceeds 10 MB.
    PayloadTooLarge,
    /// The JSON object is nested more than 32 levels deep.
    InvalidJsonDepth,
    /// A single request contains more than 1000 keys.
    TooManyKeys,
    /// The request payload contains a property that is not recognised for this endpoint.
    UnknownProperty(String),
}

/// Implement Display so ValidationError can be returned as a JSON error message.
impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidCollectionName(name) => {
                write!(f, "Invalid collection name: '{}'. Must be alphanumeric with _, - only (1-64 chars)", name)
            }
            ValidationError::InvalidKeyName(name) => {
                write!(f, "Invalid key name: '{}'. Must be alphanumeric with _, -, . only (1-256 chars)", name)
            }
            ValidationError::InvalidFieldName(name) => {
                write!(f, "Invalid field name: '{}'. Must be alphanumeric with _, -, . only (1-128 chars)", name)
            }
            ValidationError::CollectionNameTooLong => {
                write!(f, "Collection name too long (max 64 characters)")
            }
            ValidationError::KeyNameTooLong => {
                write!(f, "Key name too long (max 256 characters)")
            }
            ValidationError::PayloadTooLarge => {
                write!(f, "Payload too large (max 10MB)")
            }
            ValidationError::InvalidJsonDepth => {
                write!(f, "JSON nesting too deep (max 32 levels)")
            }
            ValidationError::TooManyKeys => {
                write!(f, "Too many keys in single request (max 1000)")
            }
            ValidationError::UnknownProperty(name) => {
                write!(f, "Unknown property: '{}'. Check the API docs for the list of supported properties for this endpoint", name)
            }
        }
    }
}

/// Mark ValidationError as a standard Rust error type.
/// Required for it to be used with the `?` operator in functions returning
/// `Result<_, Box<dyn std::error::Error>>`.
impl std::error::Error for ValidationError {}

// ─── Individual validators ────────────────────────────────────────────────────

/// Validate a collection name.
///
/// Rules:
///   - Must not be empty.
///   - Must be 1–64 characters.
///   - Must match [a-zA-Z0-9_-] (no dots, slashes, spaces, or special chars).
///   - Must not be a reserved name (admin, system, config, internal, __proto__).
pub fn validate_collection_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() {
        return Err(ValidationError::InvalidCollectionName(name.to_string()));
    }

    if name.len() > 64 {
        return Err(ValidationError::CollectionNameTooLong);
    }

    // Check against the compiled regex.
    if !COLLECTION_NAME_REGEX.is_match(name) {
        return Err(ValidationError::InvalidCollectionName(name.to_string()));
    }

    // Block reserved names that could be confused with system collections
    // or used for privilege escalation.
    if matches!(name, "admin" | "system" | "config" | "internal" | "__proto__") {
        return Err(ValidationError::InvalidCollectionName(
            format!("{} (reserved name)", name)
        ));
    }

    Ok(())
}

/// Validate a document key name.
///
/// Rules:
///   - Must not be empty.
///   - Must be 1–256 characters.
///   - Must match [a-zA-Z0-9_.-] (dots allowed for structured keys like "user.123").
pub fn validate_key_name(key: &str) -> Result<(), ValidationError> {
    if key.is_empty() {
        return Err(ValidationError::InvalidKeyName(key.to_string()));
    }

    if key.len() > 256 {
        return Err(ValidationError::KeyNameTooLong);
    }

    if !KEY_NAME_REGEX.is_match(key) {
        return Err(ValidationError::InvalidKeyName(key.to_string()));
    }

    Ok(())
}

/// Validate a field name used in projections, WHERE clauses, or joins.
///
/// Rules:
///   - Must not be empty.
///   - Must be 1–128 characters.
///   - Must match [a-zA-Z0-9_.-] (dots allowed for nested paths like "meta.logins").
///   - Dot-separated parts must not be empty (rejects "a..b" or ".field").
pub fn validate_field_name(field: &str) -> Result<(), ValidationError> {
    if field.is_empty() {
        return Err(ValidationError::InvalidFieldName(field.to_string()));
    }

    if field.len() > 128 {
        return Err(ValidationError::InvalidFieldName(
            format!("{} (too long)", field)
        ));
    }

    if !FIELD_NAME_REGEX.is_match(field) {
        return Err(ValidationError::InvalidFieldName(field.to_string()));
    }

    // Validate each dot-separated part individually.
    // This catches "a..b" (empty middle part) or ".field" (empty first part).
    for part in field.split('.') {
        if part.is_empty() {
            return Err(ValidationError::InvalidFieldName(field.to_string()));
        }
    }

    Ok(())
}

/// Validate that a JSON value is not nested more than `max_depth` levels deep.
///
/// Deeply nested JSON can cause stack overflows during recursive processing.
/// The limit of 32 levels is generous for real data but blocks malicious inputs
/// like { a: { b: { c: ... } } } with hundreds of levels.
///
/// Uses a recursive inner function — the outer function is the public API.
pub fn validate_json_depth(value: &Value, max_depth: usize) -> Result<(), ValidationError> {
    /// Inner recursive function that tracks the current depth.
    fn check_depth(value: &Value, current: usize, max: usize) -> Result<(), ValidationError> {
        // If we've exceeded the maximum depth, reject immediately.
        if current > max {
            return Err(ValidationError::InvalidJsonDepth);
        }

        match value {
            // For objects and arrays, recurse into each child with depth + 1.
            Value::Object(map) => {
                for v in map.values() {
                    check_depth(v, current + 1, max)?;
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    check_depth(v, current + 1, max)?;
                }
            }
            // Scalar values (string, number, bool, null) don't add depth.
            _ => {}
        }

        Ok(())
    }

    // Start the recursion at depth 0.
    check_depth(value, 0, max_depth)
}

/// Validate that the serialized payload does not exceed `max_size_bytes`.
///
/// Serializing to a string to measure size is slightly wasteful, but it's
/// the most accurate way to measure the actual byte count of the JSON.
pub fn validate_payload_size(payload: &Value, max_size_bytes: usize) -> Result<(), ValidationError> {
    // Serialize to a string to get the byte count.
    // unwrap_or_default() returns an empty string if serialization fails —
    // in that case the size check passes (the real error will surface later).
    let serialized = serde_json::to_string(payload).unwrap_or_default();
    if serialized.len() > max_size_bytes {
        return Err(ValidationError::PayloadTooLarge);
    }
    Ok(())
}

/// Validate that a batch operation doesn't contain more than `max_keys` keys.
///
/// Large batches can cause CPU spikes and memory pressure. The limit of 1000
/// keys per request is generous for normal use but blocks accidental or
/// malicious bulk operations.
pub fn validate_key_count(count: usize, max_keys: usize) -> Result<(), ValidationError> {
    if count > max_keys {
        return Err(ValidationError::TooManyKeys);
    }
    Ok(())
}

/// Check that every top-level key in the payload is in the `allowed` list.
///
/// This prevents clients from sending unrecognised properties that would be
/// silently ignored, which can mask typos (e.g. `"filed"` instead of `"fields"`).
/// Only top-level keys are checked — nested document data is not validated here.
pub fn validate_allowed_properties(payload: &Value, allowed: &[&str]) -> Result<(), ValidationError> {
    if let Some(obj) = payload.as_object() {
        for key in obj.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(ValidationError::UnknownProperty(key.clone()));
            }
        }
    }
    Ok(())
}

/// Run all validation checks on an incoming request payload.
///
/// This is the single entry point called by every process_* function in
/// handlers.rs. It checks everything in one pass:
///   1. Payload size (10 MB limit)
///   2. JSON nesting depth (32 levels max)
///   3. Collection name validity
///   4. Key name validity (single key, batch keys, data map keys)
///   5. Field name validity (projections, joins, WHERE clause)
///
/// Returns Ok(()) if all checks pass, or the first ValidationError found.
pub fn validate_request(payload: &Value, max_body_size: usize) -> Result<(), ValidationError> {
    // Check 1: Payload size — reject before doing any other work.
    validate_payload_size(payload, max_body_size)?;

    // Check 2: JSON depth — prevent stack overflows in recursive processing.
    validate_json_depth(payload, 32)?;

    // Check 3: Collection name — must be safe to use as a storage key.
    if let Some(collection) = payload.get("collection").and_then(|v| v.as_str()) {
        validate_collection_name(collection)?;
    }

    // Check 4a: Single or batch key lookup/delete.
    if let Some(keys) = payload.get("keys") {
        match keys {
            Value::String(key) => validate_key_name(key)?,
            Value::Array(arr) => {
                // Reject if too many keys in one request.
                validate_key_count(arr.len(), 1000)?;
                for key in arr {
                    if let Some(key_str) = key.as_str() {
                        validate_key_name(key_str)?;
                    }
                }
            }
            _ => {}
        }
    }

    // Check 4b: Data map keys in insert/update operations.
    if let Some(data) = payload.get("data") {
        if let Value::Object(map) = data {
            validate_key_count(map.len(), 1000)?;
            for key in map.keys() {
                validate_key_name(key)?;
            }
        }
    }

    // Check 5a: Field names in projection (fields: ["name", "meta.logins"]).
    if let Some(fields) = payload.get("fields").and_then(|v| v.as_array()) {
        for field in fields {
            if let Some(field_str) = field.as_str() {
                validate_field_name(field_str)?;
            }
        }
    }

    // Check 5b: Join specifications — validate collection, alias, foreign_key, fields.
    if let Some(joins) = payload.get("joins").and_then(|v| v.as_array()) {
        for join in joins {
            if let Some(join_collection) = join.get("collection").and_then(|v| v.as_str()) {
                validate_collection_name(join_collection)?;
            }
            if let Some(alias) = join.get("alias").and_then(|v| v.as_str()) {
                validate_key_name(alias)?;
            }
            if let Some(foreign_key) = join.get("foreign_key").and_then(|v| v.as_str()) {
                validate_field_name(foreign_key)?;
            }
            if let Some(join_fields) = join.get("fields").and_then(|v| v.as_array()) {
                for field in join_fields {
                    if let Some(field_str) = field.as_str() {
                        validate_field_name(field_str)?;
                    }
                }
            }
        }
    }

    // Check 5c: WHERE clause field names (top-level keys only).
    // Operator keys like $or, $and, $gt are skipped (they start with '$').
    if let Some(where_clause) = payload.get("where").and_then(|v| v.as_object()) {
        for key in where_clause.keys() {
            if !key.starts_with('$') {
                validate_field_name(key)?;
            }
        }
    }

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────
// These tests run with `cargo test` and verify the validation logic.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Valid collection names should pass without error.
    #[test]
    fn test_valid_collection_names() {
        assert!(validate_collection_name("users").is_ok());
        assert!(validate_collection_name("user_data").is_ok());
        assert!(validate_collection_name("data-2024").is_ok());
        assert!(validate_collection_name("test123").is_ok());
    }

    /// Invalid collection names should be rejected.
    #[test]
    fn test_invalid_collection_names() {
        assert!(validate_collection_name("").is_err());           // empty
        assert!(validate_collection_name("user$data").is_err());  // invalid char
        assert!(validate_collection_name("../etc/passwd").is_err()); // path traversal
        assert!(validate_collection_name("admin").is_err());      // reserved name
    }

    /// Shallow JSON should pass the depth check; deeply nested JSON should fail.
    #[test]
    fn test_json_depth() {
        let shallow = json!({"a": {"b": "c"}});
        assert!(validate_json_depth(&shallow, 10).is_ok());

        // Build a JSON object nested 50 levels deep — should fail at max 32.
        let mut deep = json!({});
        let mut current = &mut deep;
        for _ in 0..50 {
            *current = json!({"nested": {}});
            current = current.get_mut("nested").unwrap();
        }
        assert!(validate_json_depth(&deep, 32).is_err());
    }
}
