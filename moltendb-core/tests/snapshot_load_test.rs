use moltendb_core::engine::{self, DbConfig};
use serde_json::json;
use std::fs;
use std::path::Path;

#[test]
fn test_snapshot_load_with_empty_log() {
    let db_path = "test_snapshot_empty_log.log";
    let snapshot_path = "test_snapshot_empty_log.log.snapshot.bin";
    
    // Cleanup
    let _ = fs::remove_file(db_path);
    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all("backup");

    {
        // 1. Create DB and insert some data
        let db = engine::Db::open(DbConfig {
            path: db_path.to_string(),
            sync_mode: true,
            hot_threshold: 1000,
            max_body_size: 1024 * 1024,
            ..Default::default()
        }).unwrap();
        db.insert("users", vec![
            ("user1".to_string(), json!({"name": "Alice"})),
            ("user2".to_string(), json!({"name": "Bob"})),
        ]).unwrap();

        // 2. Trigger compaction to create a snapshot
        db.compact().unwrap();
        
        // Verify snapshot exists
        assert!(Path::new(snapshot_path).exists());
    }

    // 3. Manually empty the log file
    fs::write(db_path, "").unwrap();

    {
        // 4. Reopen the DB
        let db = engine::Db::open(DbConfig {
            path: db_path.to_string(),
            sync_mode: true,
            hot_threshold: 1000,
            max_body_size: 1024 * 1024,
            ..Default::default()
        }).unwrap();
        
        // 5. Try to get data
        let user1 = db.get("users", vec!["user1".to_string()]).remove("user1");
        let user2 = db.get("users", vec!["user2".to_string()]).remove("user2");

        assert!(user1.is_some(), "User1 should be loaded from snapshot");
        assert_eq!(user1.unwrap()["name"], "Alice");
        assert!(user2.is_some(), "User2 should be loaded from snapshot");
        assert_eq!(user2.unwrap()["name"], "Bob");
    }

    // Cleanup
    let _ = fs::remove_file(db_path);
    let _ = fs::remove_file(snapshot_path);
}
