use super::constants::{COLLECTION_NAME_REGEX, FIELD_NAME_REGEX, KEY_NAME_REGEX};
use super::errors::ValidationError;
use serde_json::Value;

pub fn validate_collection_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() || name.len() > 64 {
        return Err(if name.len() > 64 {
            ValidationError::CollectionNameTooLong
        } else {
            ValidationError::InvalidCollectionName(name.to_string())
        });
    }
    if !COLLECTION_NAME_REGEX.is_match(name) {
        return Err(ValidationError::InvalidCollectionName(name.to_string()));
    }
    if matches!(
        name,
        "admin" | "system" | "config" | "internal" | "__proto__"
    ) {
        return Err(ValidationError::InvalidCollectionName(format!(
            "{} (reserved name)",
            name
        )));
    }
    Ok(())
}

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

pub fn validate_field_name(field: &str) -> Result<(), ValidationError> {
    if field.is_empty() {
        return Err(ValidationError::InvalidFieldName(field.to_string()));
    }
    if field.len() > 128 {
        return Err(ValidationError::InvalidFieldName(format!(
            "{} (too long)",
            field
        )));
    }
    if !FIELD_NAME_REGEX.is_match(field) {
        return Err(ValidationError::InvalidFieldName(field.to_string()));
    }
    for part in field.split('.') {
        if part.is_empty() {
            return Err(ValidationError::InvalidFieldName(field.to_string()));
        }
    }
    Ok(())
}

pub fn validate_json_depth(value: &Value, max_depth: usize) -> Result<(), ValidationError> {
    fn check(value: &Value, current: usize, max: usize) -> Result<(), ValidationError> {
        if current > max {
            return Err(ValidationError::InvalidJsonDepth);
        }
        match value {
            Value::Object(map) => {
                for v in map.values() {
                    check(v, current + 1, max)?;
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    check(v, current + 1, max)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    check(value, 0, max_depth)
}

pub fn validate_payload_size(
    payload: &Value,
    max_size_bytes: usize,
) -> Result<(), ValidationError> {
    let serialized = serde_json::to_string(payload).unwrap_or_default();
    if serialized.len() > max_size_bytes {
        return Err(ValidationError::PayloadTooLarge);
    }
    Ok(())
}

pub fn validate_key_count(count: usize, max_keys: usize) -> Result<(), ValidationError> {
    if count > max_keys {
        return Err(ValidationError::TooManyKeys);
    }
    Ok(())
}

pub fn validate_allowed_properties(
    payload: &Value,
    allowed: &[&str],
) -> Result<(), ValidationError> {
    if let Some(obj) = payload.as_object() {
        for key in obj.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(ValidationError::UnknownProperty(key.clone()));
            }
        }
    }
    Ok(())
}

pub fn validate_request(
    payload: &Value,
    max_body_size: usize,
    max_keys_per_request: usize,
) -> Result<(), ValidationError> {
    validate_payload_size(payload, max_body_size)?;
    validate_json_depth(payload, 32)?;

    if let Some(collection) = payload.get("collection").and_then(|v| v.as_str()) {
        validate_collection_name(collection)?;
    }

    if let Some(keys) = payload.get("keys") {
        match keys {
            Value::String(key) => validate_key_name(key)?,
            Value::Array(arr) => {
                validate_key_count(arr.len(), max_keys_per_request)?;
                for key in arr {
                    if let Some(s) = key.as_str() {
                        validate_key_name(s)?;
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(data) = payload.get("data")
        && let Value::Object(map) = data
    {
        validate_key_count(map.len(), max_keys_per_request)?;
        for key in map.keys() {
            validate_key_name(key)?;
        }
    }

    if let Some(fields) = payload.get("fields").and_then(|v| v.as_array()) {
        for field in fields {
            if let Some(s) = field.as_str() {
                validate_field_name(s)?;
            }
        }
    }

    if let Some(joins) = payload.get("joins").and_then(|v| v.as_array()) {
        for join in joins {
            if let Some(c) = join.get("collection").and_then(|v| v.as_str()) {
                validate_collection_name(c)?;
            }
            if let Some(a) = join.get("alias").and_then(|v| v.as_str()) {
                validate_key_name(a)?;
            }
            if let Some(fk) = join.get("foreign_key").and_then(|v| v.as_str()) {
                validate_field_name(fk)?;
            }
            if let Some(jf) = join.get("fields").and_then(|v| v.as_array()) {
                for f in jf {
                    if let Some(s) = f.as_str() {
                        validate_field_name(s)?;
                    }
                }
            }
        }
    }

    if let Some(where_clause) = payload.get("where").and_then(|v| v.as_object()) {
        for key in where_clause.keys() {
            if !key.starts_with('$') {
                validate_field_name(key)?;
            }
        }
    }

    Ok(())
}
