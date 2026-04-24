use moltendb_core::engine::Db;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread")]
async fn test_schema_enforcement() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Db::open(db_path.to_str().unwrap(), false, false, 50000, 100, 60, 10 * 1024 * 1024, None).unwrap();

    // 1. Set a schema for 'users'
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer", "minimum": 0 }
        },
        "required": ["name"]
    });
    db.set_schema("users", schema).unwrap();

    // 2. Insert valid document
    let valid_doc = json!({ "name": "Alice", "age": 30 });
    db.insert_batch("users", vec![("u1".to_string(), valid_doc)]).unwrap();

    // 3. Insert invalid document (wrong type)
    let invalid_doc = json!({ "name": "Bob", "age": "thirty" });
    let result = db.insert_batch("users", vec![("u2".to_string(), invalid_doc)]);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Schema Validation Error"));

    // 4. Insert invalid document (missing required field)
    let missing_field = json!({ "age": 25 });
    let result = db.insert_batch("users", vec![("u3".to_string(), missing_field)]);
    assert!(result.is_err());

    // 5. Update with invalid data
    let invalid_update = json!({ "age": -5 });
    let result = db.update("users", "u1", invalid_update);
    assert!(result.is_err());

    // 6. Update with valid data
    let valid_update = json!({ "age": 31 });
    let result = db.update("users", "u1", valid_update).unwrap();
    assert!(result);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_schema_persistence() {
    let dir = tempdir().unwrap();
    let db_path_buf = dir.path().join("test_persistence.db");
    let db_path = db_path_buf.to_str().unwrap();

    {
        let db = Db::open(db_path, false, false, 50000, 100, 60, 10 * 1024 * 1024, None).unwrap();
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "number" }
            }
        });
        db.set_schema("items", schema).unwrap();
    }

    // Re-open and verify schema is still there
    {
        let db = Db::open(db_path, false, false, 50000, 100, 60, 10 * 1024 * 1024, None).unwrap();
        let invalid_doc = json!({ "count": "many" });
        let result = db.insert_batch("items", vec![("i1".to_string(), invalid_doc)]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Schema Validation Error"));
    }
}
