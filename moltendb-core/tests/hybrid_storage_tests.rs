use moltendb_core::engine::Db;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn open_db(threshold: usize) -> Db {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("moltendb_hybrid_test_{}.log", id));
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    Db::open(path.to_str().unwrap(), true, false, threshold, 100, 60, 10485760, None, None).expect("Failed to open db")
}

#[test]
fn test_hot_cold_transition() {
    // Set a very low threshold to trigger eviction early
    let db = open_db(2);
    
    // Insert 3 documents into "items" collection
    db.insert_batch("items", vec![
        ("k1".to_string(), json!({"v": 1})),
        ("k2".to_string(), json!({"v": 2})),
        ("k3".to_string(), json!({"v": 3})),
    ]).unwrap();
    
    // Explicitly trigger eviction (threshold 2)
    let evicted = db.evict_collection("items", 2).expect("Eviction failed");
    // Depending on implementation, it should evict at least 1 document if count is 3 and limit is 2.
    assert!(evicted > 0, "Should have evicted some documents");
    
    // Verify we can still get all documents (transparent fetch)
    let v1 = db.get("items", "k1").unwrap();
    let v2 = db.get("items", "k2").unwrap();
    let v3 = db.get("items", "k3").unwrap();
    
    assert_eq!(v1["v"], 1);
    assert_eq!(v2["v"], 2);
    assert_eq!(v3["v"], 3);
}

#[test]
fn test_configurable_threshold() {
    let db_small = open_db(10);
    assert_eq!(db_small.hot_threshold, 10);
    
    let db_large = open_db(100000);
    assert_eq!(db_large.hot_threshold, 100000);
}
