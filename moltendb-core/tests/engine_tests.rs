use moltendb_core::engine::{Db, DbConfig};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn open_db() -> Db {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("moltendb_core_test_{}.log", id));
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    Db::open(DbConfig {
        path: path.to_str().unwrap().to_string(),
        sync_mode: true,
        ..Default::default()
    })
    .expect("Failed to open db")
}

#[test]
fn test_basic_crud() {
    let db = open_db();

    // Insert
    let items = vec![
        ("u1".to_string(), json!({"name": "Alice", "age": 30})),
        ("u2".to_string(), json!({"name": "Bob", "age": 25})),
    ];
    db.insert("users", items).expect("Insert failed");

    // Get
    let u1 = db
        .get("users", vec!["u1".to_string()])
        .remove("u1")
        .expect("Failed to get u1");
    assert_eq!(u1["name"], "Alice");

    // Update
    db.update("users", "u1", json!({"age": 31}))
        .expect("Update failed");
    let u1_updated = db
        .get("users", vec!["u1".to_string()])
        .remove("u1")
        .unwrap();
    assert_eq!(u1_updated["age"], 31);
    assert_eq!(u1_updated["name"], "Alice"); // Ensure name is preserved (Db::update is partial)

    // Delete
    db.delete("users", vec!["u2".to_string()])
        .expect("Delete failed");
    assert!(db.get("users", vec!["u2".to_string()]).is_empty());
}

#[test]
fn test_batch_operations() {
    let db = open_db();

    let items = (0..100)
        .map(|i| (format!("k{}", i), json!({"v": i})))
        .collect::<Vec<_>>();

    db.insert("bench", items).expect("Batch insert failed");

    let all = db.get_filtered(moltendb_core::engine::GetFilteredParams {
        collection: "bench",
        predicate: |_, _| true,
        offset: 0,
        count: None,
        default_order_asc: true,
        has_where: false,
    });
    assert_eq!(all.len(), 100);

    let keys = vec!["k0".to_string(), "k50".to_string(), "k99".to_string()];
    let batch = db.get("bench", keys);
    assert_eq!(batch.len(), 3);
    assert_eq!(batch["k50"]["v"], 50);
}

#[test]
fn test_persistence() {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path_buf = std::env::temp_dir().join(format!("moltendb_core_persist_{}.log", id));
    let path = path_buf.to_str().unwrap().to_string();
    if path_buf.exists() {
        let _ = std::fs::remove_file(&path_buf);
    }

    {
        let db = Db::open(DbConfig {
            path: path.clone(),
            sync_mode: true,
            ..Default::default()
        })
        .unwrap();
        db.insert("items", vec![("k1".to_string(), json!({"val": 100}))])
            .unwrap();
    }

    // Reopen
    let db2 = Db::open(DbConfig {
        path: path.clone(),
        sync_mode: true,
        ..Default::default()
    })
    .unwrap();
    let val = db2
        .get("items", vec!["k1".to_string()])
        .remove("k1")
        .unwrap();
    assert_eq!(val["val"], 100);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_compaction() {
    let db = open_db();

    // Write some data
    db.insert("c", vec![("k1".to_string(), json!({"v": 1}))])
        .unwrap();
    db.update("c", "k1", json!({"v": 2})).unwrap();
    db.update("c", "k1", json!({"v": 3})).unwrap();

    db.compact().expect("Compaction failed");

    let val = db.get("c", vec!["k1".to_string()]).remove("k1").unwrap();
    assert_eq!(val["v"], 3);
}
