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

#[tokio::test(flavor = "multi_thread")]
async fn test_delete_filtered() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.log");
    let db = Db::open(DbConfig {
        path: path.to_str().unwrap().to_string(),
        max_body_size: 1024 * 1024,
        ..Default::default()
    }).unwrap();

    // Insert a mix of documents.
    db.insert("items", vec![
        ("a".to_string(), json!({"role": "guest", "score": 10})),
        ("b".to_string(), json!({"role": "admin", "score": 20})),
        ("c".to_string(), json!({"role": "guest", "score": 30})),
        ("d".to_string(), json!({"role": "editor", "score": 40})),
    ]).unwrap();

    // Delete all guests.
    let deleted = db.delete_filtered("items", |doc| {
        doc.get("role").and_then(|v| v.as_str()) == Some("guest")
    }, None).unwrap();
    assert_eq!(deleted, 2);

    // Only admin and editor should remain.
    let remaining: std::collections::HashMap<_, _> = db.get_all("items", 0, None).into_iter().collect();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.contains_key("b"));
    assert!(remaining.contains_key("d"));
    assert!(!remaining.contains_key("a"));
    assert!(!remaining.contains_key("c"));

    // delete_filtered on a non-existent collection returns 0, not an error.
    let deleted = db.delete_filtered("nonexistent", |_| true, None).unwrap();
    assert_eq!(deleted, 0);
}
