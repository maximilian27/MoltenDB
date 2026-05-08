// Previously tested hot/cold eviction — now all documents are always in RAM.
// This file keeps basic insert/get coverage to ensure the storage path works.

use moltendb_core::engine::{Db, DbConfig};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn open_db() -> Db {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("moltendb_hybrid_test_{}.log", id));
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    Db::open(DbConfig {
        path: path.to_str().unwrap().to_string(),
        sync_mode: true,
        ..Default::default()
    }).expect("Failed to open db")
}

#[tokio::test(flavor = "multi_thread")]
async fn test_all_docs_in_memory() {
    let db = open_db();

    db.insert("items", vec![
        ("k1".to_string(), json!({"v": 1})),
        ("k2".to_string(), json!({"v": 2})),
        ("k3".to_string(), json!({"v": 3})),
    ]).unwrap();

    let v1 = db.get("items", vec!["k1".to_string()]).remove("k1").unwrap();
    let v2 = db.get("items", vec!["k2".to_string()]).remove("k2").unwrap();
    let v3 = db.get("items", vec!["k3".to_string()]).remove("k3").unwrap();

    assert_eq!(v1["v"], 1);
    assert_eq!(v2["v"], 2);
    assert_eq!(v3["v"], 3);
}
