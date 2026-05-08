use moltendb_core::engine::{Db, DbConfig, DbError};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread")]
async fn test_insert_batch_conflict() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.log");
    let db = Db::open(DbConfig {
        path: path.to_str().unwrap().to_string(),
        max_body_size: 1024 * 1024,
        ..Default::default()
    }).unwrap();

    // 1. Initial insert
    db.insert("users", vec![("u1".to_string(), json!({"name": "Alice"}))]).unwrap();
    
    // Check version
    let doc = db.get("users", vec!["u1".to_string()]).remove("u1").unwrap();
    assert_eq!(doc.get("_v").unwrap().as_u64().unwrap(), 1);

    // 2. Conflict: Try to insert with _v=1 (should be rejected because stored is 1)
    let res = db.insert("users", vec![("u1".to_string(), json!({"name": "Alice Updated", "_v": 1}))]);
    assert!(matches!(res, Err(DbError::Conflict)));

    // 3. Success: Insert with _v=2
    db.insert("users", vec![("u1".to_string(), json!({"name": "Alice Updated", "_v": 2}))]).unwrap();
    let doc = db.get("users", vec!["u1".to_string()]).remove("u1").unwrap();
    assert_eq!(doc.get("_v").unwrap().as_u64().unwrap(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_update_conflict_guard() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.log");
    let db = Db::open(DbConfig {
        path: path.to_str().unwrap().to_string(),
        max_body_size: 1024 * 1024,
        ..Default::default()
    }).unwrap();

    // 1. Initial insert
    db.insert("users", vec![("u1".to_string(), json!({"name": "Alice"}))]).unwrap();
    
    // 2. Conflict: Update with wrong guard version
    let res = db.update("users", "u1", json!({"role": "admin", "_v": 10}));
    assert!(matches!(res, Err(DbError::Conflict)));

    // 3. Success: Update with correct guard version
    let res = db.update("users", "u1", json!({"role": "admin", "_v": 1})).unwrap();
    assert!(res);
    let doc = db.get("users", vec!["u1".to_string()]).remove("u1").unwrap();
    assert_eq!(doc.get("_v").unwrap().as_u64().unwrap(), 2);
    assert_eq!(doc.get("role").unwrap().as_str().unwrap(), "admin");

    // 4. Success: Update WITHOUT guard
    let res = db.update("users", "u1", json!({"active": true})).unwrap();
    assert!(res);
    let doc = db.get("users", vec!["u1".to_string()]).remove("u1").unwrap();
    assert_eq!(doc.get("_v").unwrap().as_u64().unwrap(), 3);
}
