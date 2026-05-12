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

    // 1. Initial insert — engine always sets _v=1 for new documents.
    db.insert("users", vec![("u1".to_string(), json!({"name": "Alice"}))]).unwrap();
    let doc = db.get("users", vec!["u1".to_string()]).remove("u1").unwrap();
    assert_eq!(doc.get("_v").unwrap().as_u64().unwrap(), 1);

    // 2. Overwrite — engine increments _v automatically; client does not supply it.
    db.insert("users", vec![("u1".to_string(), json!({"name": "Alice Updated"}))]).unwrap();
    let doc = db.get("users", vec!["u1".to_string()]).remove("u1").unwrap();
    assert_eq!(doc.get("_v").unwrap().as_u64().unwrap(), 2);

    // 3. Another overwrite — _v keeps incrementing.
    db.insert("users", vec![("u1".to_string(), json!({"name": "Alice v3"}))]).unwrap();
    let doc = db.get("users", vec!["u1".to_string()]).remove("u1").unwrap();
    assert_eq!(doc.get("_v").unwrap().as_u64().unwrap(), 3);
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
