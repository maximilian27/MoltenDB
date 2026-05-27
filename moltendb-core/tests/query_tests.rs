use moltendb_core::engine::{Db, DbConfig};
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
    Db::open(DbConfig {
        path: path.to_str().unwrap().to_string(),
        sync_mode: true,
        ..Default::default()
    }).expect("Failed to open db")
}

fn seed_data(db: &Db) {
    let items = vec![
        ("p1".to_string(), json!({"type": "fruit", "name": "apple", "price": 1.5})),
        ("p2".to_string(), json!({"type": "fruit", "name": "banana", "price": 1.0})),
        ("p3".to_string(), json!({"type": "vegetable", "name": "carrot", "price": 0.8})),
        ("p4".to_string(), json!({"type": "vegetable", "name": "broccoli", "price": 1.2})),
    ];
    db.insert("products", items).unwrap();
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
    let (_, results) = process_get(&db, &payload, 1024 * 1024, 1000);
    assert_eq!(results.as_array().unwrap().len(), 2);

    // Numeric comparison
    let payload = json!({
        "collection": "products",
        "where": {"price": {"$gt": 1.1}}
    });
    let (_, results) = process_get(&db, &payload, 1024 * 1024, 1000);
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

    let (_, results) = process_get(&db, &payload, 1024 * 1024, 1000);
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

    let (_, results) = process_get(&db, &payload, 1024 * 1024, 1000);
    let arr = results.as_array().unwrap();
    let first = &arr[0];
    assert!(first.get("name").is_some());
    assert!(first.get("type").is_none());
}

#[test]
fn test_query_deep_pagination_zero_string_scan() {
    let db = open_db();
    
    // Insert 6000 small items to test both bounded heap and flat vector select_nth_unstable_by paths
    let items: Vec<(String, serde_json::Value)> = (0..6000)
        .map(|i| {
            (
                format!("item_{}", i),
                json!({
                    "val": i,
                    "type": "test",
                }),
            )
        })
        .collect();
    
    db.insert("items", items).unwrap();
    
    // 1. Small offset + limit (<= 5000), using bounded heap path
    let payload_small = json!({
        "collection": "items",
        "where": {"type": "test"},
        "offset": 10,
        "count": 5,
    });
    let (_, results_small) = process_get(&db, &payload_small, 1024 * 1024, 1000);
    let arr_small = results_small.as_array().unwrap();
    assert_eq!(arr_small.len(), 5);
    
    // 2. Large offset + limit (> 5000), using flat vec select_nth_unstable_by path
    let payload_large = json!({
        "collection": "items",
        "where": {"type": "test"},
        "offset": 5500,
        "count": 10,
    });
    let (_, results_large) = process_get(&db, &payload_large, 1024 * 1024, 1000);
    let arr_large = results_large.as_array().unwrap();
    assert_eq!(arr_large.len(), 10);
}
