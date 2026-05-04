use moltendb_core::engine::{Db, DbConfig};
use moltendb_core::engine::EncryptedStorage;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn get_temp_path() -> String {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("moltendb_encryption_test_{}.log", id));
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    path.to_str().unwrap().to_string()
}

#[test]
fn test_encryption_automatic_if_key_provided() {
    let path = get_temp_path();
    let password = "super-secret-password";
    // Derive key
    let key = EncryptedStorage::derive_key(password, &path);

    // 1. Open with encryption
    {
        let db = Db::open(DbConfig {
            path: path.clone(),
            sync_mode: true,
            encryption_key: Some(key),
            ..Default::default()
        }).expect("Failed to open encrypted db");
        db.insert("secrets", vec![("top".to_string(), json!({"data": "hidden"}))]).unwrap();
    }

    // 2. Try to open without encryption (should fail to replay or see garbage)
    let db_no_enc = Db::open(DbConfig {
        path: path.clone(),
        sync_mode: true,
        ..Default::default()
    }).expect("Failed to open db without encryption");
    let val = db_no_enc.get("secrets", vec!["top".to_string()]).remove("top");
    assert!(val.is_none(), "Should not be able to read encrypted data without key");

    // 3. Open again with encryption
    let db_enc = Db::open(DbConfig {
        path: path.clone(),
        sync_mode: true,
        encryption_key: Some(key),
        ..Default::default()
    }).expect("Failed to reopen encrypted db");
    let val_enc = db_enc.get("secrets", vec!["top".to_string()]).remove("top");
    if val_enc.is_none() {
        panic!("Should read encrypted data with key, but got None. Path: {}", path);
    }
    let val_enc = val_enc.unwrap();
    assert_eq!(val_enc["data"], "hidden");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_plain_json_if_no_key_provided() {
    let path = get_temp_path();

    // 1. Open without encryption
    {
        let db = Db::open(DbConfig {
            path: path.clone(),
            sync_mode: true,
            ..Default::default()
        }).expect("Failed to open plain db");
        db.insert("public", vec![("info".to_string(), json!({"data": "visible"}))]).unwrap();
    }

    // 2. Verify it's plain JSON on disk
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("\"data\":\"visible\""), "Log should contain plain JSON");
    assert!(content.contains("\"cmd\":\"INSERT\""), "Log should contain INSERT command");

    // 3. Reopen and verify
    let db2 = Db::open(DbConfig {
        path: path.clone(),
        sync_mode: true,
        ..Default::default()
    }).expect("Failed to reopen plain db");
    let val = db2.get("public", vec!["info".to_string()]).remove("info").unwrap();
    assert_eq!(val["data"], "visible");

    let _ = std::fs::remove_file(&path);
}
