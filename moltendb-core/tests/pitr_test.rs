use moltendb_core::engine::{Db, DbConfig};
use serde_json::json;

#[test]
fn test_pitr_timestamp_metadata() {
    let temp_dir = tempfile::tempdir().unwrap();
    let log_path = temp_dir.path().join("pitr_test.log");
    let log_path_str = log_path.to_str().unwrap();
    
    // 1. Open DB and write some data
    let db = Db::open(DbConfig {
        path: log_path_str.to_string(),
        sync_mode: true,
        ..Default::default()
    }).unwrap();
    
    let t_start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    
    db.insert("test", vec![("k1".to_string(), json!({"v": 1}))]).unwrap();
    
    let t_end = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // 2. Read the log via the storage API to check for _t
    let sync_storage_check = moltendb_core::engine::SyncDiskStorage::new(log_path_str).unwrap();
    let entries_check = moltendb_core::engine::StorageBackend::read_log(&sync_storage_check).unwrap();

    // There should be at least 3 entries: TX_BEGIN, INSERT, TX_COMMIT
    assert!(entries_check.len() >= 3, "Log should have at least 3 entries for a batch insert");

    for entry in &entries_check {
        assert!(entry._t > 0, "Every log entry must have a _t field");
        assert!(entry._t >= t_start, "Timestamp {} should be >= start time {}", entry._t, t_start);
        assert!(entry._t <= t_end, "Timestamp {} should be <= end time {}", entry._t, t_end);
    }
    
    // 3. Re-open the database from the log to verify it recovered with _t
    drop(db);
    let db2 = Db::open(DbConfig {
        path: log_path_str.to_string(),
        sync_mode: true,
        ..Default::default()
    }).unwrap();
    let _k1 = db2.get("test", vec!["k1".to_string()]).remove("k1").expect("k1 should be recovered");
    
    // Instead, we verify that PITR recovery works which relies on _t.
    let sync_storage = moltendb_core::engine::SyncDiskStorage::new(log_path_str).unwrap();
    let recovered = Db::recover_to(&sync_storage, Some(t_end), None).unwrap();
    assert!(recovered.iter().any(|e| e.key == "k1" && e._t > 0));
}
