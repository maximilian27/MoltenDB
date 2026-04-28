use moltendb_core::engine::{Db, DbConfig, LogEntry};
use serde_json::json;

#[tokio::test]
async fn test_batch_atomicity() {
    let log_path = "test_atomicity.log";
    let _ = std::fs::remove_file(log_path);

    // 1. Create a DB and write a partial batch manually to the log
    {
        let db = Db::open(DbConfig {
            path: log_path.to_string(),
            sync_mode: true,
            rate_limit_window: 1,
            max_body_size: 1024 * 1024,
            ..Default::default()
        }).unwrap();
        let collection = "test";
        
        // Use the storage directly to simulate a crash (no TX_COMMIT)
        let tx_id = "test-tx-123".to_string();
        db.storage.write_entry(&LogEntry::new(
            "TX_BEGIN".into(),
            collection.into(),
            tx_id.clone(),
            json!(null),
        )).unwrap();

        db.storage.write_entry(&LogEntry::new(
            "INSERT".into(),
            collection.into(),
            "key1".into(),
            json!({"data": "should not exist"}),
        )).unwrap();

        // We "crash" here — no TX_COMMIT is written.
    }

    // 2. Reopen the DB. The partial batch should NOT be present.
    {
        let db = Db::open(DbConfig {
            path: log_path.to_string(),
            sync_mode: true,
            rate_limit_window: 1,
            max_body_size: 1024 * 1024,
            ..Default::default()
        }).unwrap();
        assert!(db.get("test", "key1").is_none(), "Key1 should not exist because transaction was never committed");
    }

    // 3. Write a full batch using the proper API
    {
        let db = Db::open(DbConfig {
            path: log_path.to_string(),
            sync_mode: true,
            rate_limit_window: 1,
            max_body_size: 1024 * 1024,
            ..Default::default()
        }).unwrap();
        let items = vec![
            ("key2".to_string(), json!({"data": "should exist"})),
            ("key3".to_string(), json!({"data": "also should exist"})),
        ];
        db.insert_batch("test", items).unwrap();
        
        assert!(db.get("test", "key2").is_some());
        assert!(db.get("test", "key3").is_some());
    }

    // 4. Reopen again. The full batch should be present.
    {
        let db = Db::open(DbConfig {
            path: log_path.to_string(),
            sync_mode: true,
            rate_limit_window: 1,
            max_body_size: 1024 * 1024,
            ..Default::default()
        }).unwrap();
        assert!(db.get("test", "key2").is_some(), "Key2 should exist after proper commit");
        assert!(db.get("test", "key3").is_some(), "Key3 should exist after proper commit");
        assert!(db.get("test", "key1").is_none(), "Key1 should still not exist");
    }

    let _ = std::fs::remove_file(log_path);
    let _ = std::fs::remove_file(format!("{}.snapshot.bin", log_path));
}
