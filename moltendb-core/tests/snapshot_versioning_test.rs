use moltendb_core::engine::Db;
use serde_json::json;
use std::fs;
use std::thread::sleep;
use std::time::Duration;

#[test]
fn test_snapshot_versioning() {
    let temp_dir = tempfile::tempdir().unwrap();
    let log_path = temp_dir.path().join("test_versioning.log");
    let log_path_str = log_path.to_str().unwrap();
    
    // 1. Open DB and write some data
    let db = Db::open(log_path_str, true, false, 50000, 100, 60, 10485760, None, None).unwrap();
    db.insert_batch("test", vec![("k1".to_string(), json!({"v": 1}))]).unwrap();
    
    // 2. Compact for the first time -> creates snapshot.bin
    db.compact().expect("First compaction failed");
    
    let snapshot_path = temp_dir.path().join("test_versioning.log.snapshot.bin");
    assert!(snapshot_path.exists(), "Snapshot should exist after first compaction");
    
    let backup_dir = temp_dir.path().join("backup");
    assert!(!backup_dir.exists(), "Backup dir should not exist yet");

    // Sleep a bit to ensure different timestamp if needed (though Unix seconds might be same)
    sleep(Duration::from_secs(1));

    // 3. Write more data and compact again -> should move first snapshot to backup/
    db.insert_batch("test", vec![("k2".to_string(), json!({"v": 2}))]).unwrap();
    db.compact().expect("Second compaction failed");
    
    assert!(snapshot_path.exists(), "Current snapshot should still exist");
    assert!(backup_dir.exists(), "Backup directory should have been created");
    
    let backups: Vec<_> = fs::read_dir(&backup_dir).unwrap().collect();
    assert_eq!(backups.len(), 1, "There should be one backup file");
    
    let backup_file = backups[0].as_ref().unwrap();
    let backup_name = backup_file.file_name().into_string().unwrap();
    assert!(backup_name.starts_with("test_versioning.log.snapshot.bin"), "Backup name should start with snapshot name");
    assert!(backup_name.ends_with(".bak"), "Backup name should end with .bak");

    // 4. Third compaction
    sleep(Duration::from_secs(1));
    db.insert_batch("test", vec![("k3".to_string(), json!({"v": 3}))]).unwrap();
    db.compact().expect("Third compaction failed");
    
    let backups: Vec<_> = fs::read_dir(&backup_dir).unwrap().collect();
    assert_eq!(backups.len(), 2, "There should be two backup files now");
}
