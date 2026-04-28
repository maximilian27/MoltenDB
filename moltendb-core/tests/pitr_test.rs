use moltendb_core::engine::Db;
use serde_json::json;
use std::fs;

#[test]
fn test_pitr_timestamp_metadata() {
    let temp_dir = tempfile::tempdir().unwrap();
    let log_path = temp_dir.path().join("pitr_test.log");
    let log_path_str = log_path.to_str().unwrap();
    
    // 1. Open DB and write some data
    let db = Db::open(log_path_str, true, false, 50000, 100, 60, 10485760, None, None).unwrap();
    
    let t_start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    
    db.insert_batch("test", vec![("k1".to_string(), json!({"v": 1}))]).unwrap();
    
    let t_end = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // 2. Read the log file manually to check for _t
    let log_content = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = log_content.trim().split('\n').collect();
    
    // There should be at least 3 lines: TX_BEGIN, INSERT, TX_COMMIT
    assert!(lines.len() >= 3, "Log should have at least 3 lines for a batch insert");
    
    for line in lines {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(entry.get("_t").is_some(), "Every log entry must have a _t field");
        
        let t = entry["_t"].as_u64().unwrap();
        assert!(t >= t_start, "Timestamp {} should be >= start time {}", t, t_start);
        assert!(t <= t_end, "Timestamp {} should be <= end time {}", t, t_end);
    }
    
    // 3. Compact and check if _t is preserved
    db.compact().unwrap();
    
    let log_content_after = fs::read_to_string(&log_path).unwrap();
    let lines_after: Vec<&str> = log_content_after.trim().split('\n').collect();
    
    // After compaction, we should have one INSERT (and maybe others like SCHEMA if enabled)
    // Actually our compaction in mod.rs preserves _t if it was loaded from log.
    // In this test, k1 was Hot, so it got a NEW timestamp during compaction.
    
    let found_k1 = lines_after.iter().any(|line| {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        entry["key"] == "k1" && entry.get("_t").is_some()
    });
    
    assert!(found_k1, "k1 should be present after compaction with a _t field");
}
