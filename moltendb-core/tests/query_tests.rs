use moltendb_core::engine::Db;
use moltendb_core::handlers::process_get;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn open_db() -> Db {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("moltendb_query_test_{}.log", id));
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    Db::open(path.to_str().unwrap(), true, false, 50000, 100, 60, 10485760, None, None).expect("Failed to open db")
}

fn seed_data(db: &Db) {
    let items = vec![
        ("p1".to_string(), json!({"type": "fruit", "name": "apple", "price": 1.5})),
        ("p2".to_string(), json!({"type": "fruit", "name": "banana", "price": 1.0})),
        ("p3".to_string(), json!({"type": "vegetable", "name": "carrot", "price": 0.8})),
        ("p4".to_string(), json!({"type": "vegetable", "name": "broccoli", "price": 1.2})),
    ];
    db.insert_batch("products", items).unwrap();
}

#[test]
fn test_query_filtering() {
    let db = open_db();
    seed_data(&db);
    
    // Exact match
    let payload = json!({
        "collection": "products",
        "where": {"type": "fruit"}
    });
    let (_, results) = process_get(&db, &payload, 1024 * 1024);
    assert_eq!(results.as_array().unwrap().len(), 2);
    
    // Numeric comparison
    let payload = json!({
        "collection": "products",
        "where": {"price": {"$gt": 1.1}}
    });
    let (_, results) = process_get(&db, &payload, 1024 * 1024);
    assert_eq!(results.as_array().unwrap().len(), 2); // apple (1.5) and broccoli (1.2)
}

#[test]
fn test_query_sorting_pagination() {
    let db = open_db();
    seed_data(&db);
    
    let payload = json!({
        "collection": "products",
        "sort": [{"field": "price", "order": "asc"}],
        "count": 2
    });
    
    let (_, results) = process_get(&db, &payload, 1024 * 1024);
    let arr = results.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "carrot"); // 0.8
    assert_eq!(arr[1]["name"], "banana"); // 1.0
}

#[test]
fn test_query_projection() {
    let db = open_db();
    seed_data(&db);
    
    let payload = json!({
        "collection": "products",
        "fields": ["name"]
    });
    
    let (_, results) = process_get(&db, &payload, 1024 * 1024);
    let arr = results.as_array().unwrap();
    let first = &arr[0];
    assert!(first.get("name").is_some());
    assert!(first.get("type").is_none());
}
