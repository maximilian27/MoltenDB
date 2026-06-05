#[cfg(test)]
mod tests {
    use super::super::validation::*;
    use serde_json::json;

    #[test]
    fn test_valid_collection_names() {
        assert!(validate_collection_name("users").is_ok());
        assert!(validate_collection_name("user_data").is_ok());
        assert!(validate_collection_name("data-2024").is_ok());
        assert!(validate_collection_name("test123").is_ok());
    }

    #[test]
    fn test_invalid_collection_names() {
        assert!(validate_collection_name("").is_err());
        assert!(validate_collection_name("user$data").is_err());
        assert!(validate_collection_name("../etc/passwd").is_err());
        assert!(validate_collection_name("admin").is_err());
    }

    #[test]
    fn test_json_depth() {
        let shallow = json!({"a": {"b": "c"}});
        assert!(validate_json_depth(&shallow, 10).is_ok());

        let mut deep = json!({});
        let mut current = &mut deep;
        for _ in 0..50 {
            *current = json!({"nested": {}});
            current = current.get_mut("nested").unwrap();
        }
        assert!(validate_json_depth(&deep, 32).is_err());
    }
}
