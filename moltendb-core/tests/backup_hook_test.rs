use moltendb_core::engine::{Db, DbConfig};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_post_backup_hook_execution() {
    let log_path = "target/test_backup_hook.log";
    let hook_output = "target/hook_executed.txt";
    let script_path = if cfg!(target_os = "windows") {
        "target/test_hook.ps1"
    } else {
        "target/test_hook.sh"
    };
    
    // Cleanup
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.snapshot.bin", log_path));
    let _ = fs::remove_file(hook_output);
    let _ = fs::remove_file(script_path);
    
    // Create a script that writes the first argument to hook_output
    if cfg!(target_os = "windows") {
        fs::write(script_path, format!("param($path)\n[System.IO.File]::WriteAllText('{}', $path)", hook_output)).unwrap();
    } else {
        fs::write(script_path, format!("#!/bin/sh\necho -n \"$1\" > {}", hook_output)).unwrap();
        // No need for chmod here as we call it with `sh script_path`
    }
    
    // 1. Open DB with the hook
    let db = Db::open(DbConfig {
        path: log_path.to_string(),
        sync_mode: true,
        post_backup_script: Some(script_path.to_string()),
        ..Default::default()
    }).expect("Failed to open db");
    
    // 2. Insert some data so there is something to compact
    db.insert_batch("test", vec![("k1".to_string(), json!({"v": 1}))]).unwrap();
    
    // 3. Trigger compaction (which triggers the hook)
    db.compact().expect("Compaction failed");
    
    // 4. Wait a bit for the background hook to execute
    let mut success = false;
    for _ in 0..10 {
        if Path::new(hook_output).exists() {
            success = true;
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    
    assert!(success, "Hook output file was not created");
    
    // 5. Verify the content contains the snapshot path
    let content = fs::read_to_string(hook_output).expect("Failed to read hook output");
    let expected_snapshot_path = fs::canonicalize(format!("{}.snapshot.bin", log_path))
        .expect("Failed to canonicalize snapshot path")
        .to_string_lossy()
        .to_string();
    
    // On Windows, echo might add trailing spaces or use a different newline
    assert!(content.trim().contains(&expected_snapshot_path) || content.trim().contains("test_backup_hook.log.snapshot.bin"), 
            "Hook output did not contain the expected snapshot path. Content: {}", content);
            
    // Cleanup
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.snapshot.bin", log_path));
    let _ = fs::remove_file(hook_output);
    let _ = fs::remove_file(script_path);
}
