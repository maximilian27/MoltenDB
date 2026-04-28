use moltendb_core::engine;
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
        let db = engine::Db::open(db_path, true, false, 1000, 0, 0, 1024 * 1024, None, None).unwrap();
        db.insert_batch("users", vec![
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
        let db = engine::Db::open(db_path, true, false, 1000, 0, 0, 1024 * 1024, None, None).unwrap();
        
        // 5. Try to get data
        let user1 = db.get("users", "user1");
        let user2 = db.get("users", "user2");

        assert!(user1.is_some(), "User1 should be loaded from snapshot");
        assert_eq!(user1.unwrap()["name"], "Alice");
        assert!(user2.is_some(), "User2 should be loaded from snapshot");
        assert_eq!(user2.unwrap()["name"], "Bob");
    }

    // Cleanup
    let _ = fs::remove_file(db_path);
    let _ = fs::remove_file(snapshot_path);
}
