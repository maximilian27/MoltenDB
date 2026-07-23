/// MoltenDB core integration test suite
/// Tests all handler operations using an in-memory database backed by a temp file.
use moltendb_core::{engine, handlers};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;


// ─── Helpers ──────────────────────────────────────────────────────────────────

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

const TEST_MAX_BODY: usize = 10 * 1024 * 1024;
const TEST_MAX_KEYS: usize = 1000;

/// Open a fresh in-memory database backed by a unique temp file.
fn open_db() -> engine::Db {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!("target/test_db_{}.log", id);
    let _ = std::fs::remove_file(&path);
    engine::Db::open(engine::DbConfig {
        path,
        sync_mode: true,
        rate_limit_requests: None,
        rate_limit_window: None,
        max_body_size: TEST_MAX_BODY,
        max_keys_per_request: TEST_MAX_KEYS,
        encryption_key: None,
        in_memory: true,
    })
    .expect("open db")
}

/// Seed the three standard collections used by most tests.
fn seed(db: &engine::Db) {
    set(
        db,
        json!({
            "collection": "memory",
            "data": {
                "mem1": { "capacity_gb": 8,  "type": "LPDDR5", "speed_mhz": 4266, "upgradeable": false },
                "mem2": { "capacity_gb": 16, "type": "LPDDR5", "speed_mhz": 4266, "upgradeable": false },
                "mem3": { "capacity_gb": 32, "type": "DDR5",   "speed_mhz": 5600, "upgradeable": true  },
                "mem4": { "capacity_gb": 64, "type": "DDR5",   "speed_mhz": 5600, "upgradeable": true  },
                "mem5": { "capacity_gb": 36, "type": "Unified","speed_mhz": 6400, "upgradeable": false }
            }
        }),
    );
    set(
        db,
        json!({
            "collection": "display",
            "data": {
                "dsp1": { "size_inch": 13.3, "resolution": "2560x1600", "panel": "IPS",      "refresh_hz": 60,  "hdr": false },
                "dsp2": { "size_inch": 14.0, "resolution": "2880x1800", "panel": "OLED",     "refresh_hz": 90,  "hdr": true  },
                "dsp3": { "size_inch": 15.6, "resolution": "1920x1080", "panel": "IPS",      "refresh_hz": 144, "hdr": false },
                "dsp4": { "size_inch": 16.2, "resolution": "3456x2234", "panel": "Mini-LED", "refresh_hz": 120, "hdr": true  },
                "dsp5": { "size_inch": 14.0, "resolution": "2560x1600", "panel": "IPS",      "refresh_hz": 165, "hdr": false }
            }
        }),
    );
    set(
        db,
        json!({
            "collection": "laptops",
            "data": {
                "lp1": { "brand": "Lenovo",    "model": "ThinkPad X1 Carbon", "price": 1499, "in_stock": true,  "memory_id": "mem2", "display_id": "dsp2", "tags": ["business","ultrabook","lightweight"], "specs": { "cpu": { "brand": "Intel", "cores": 12, "ghz": 3.5 }, "battery_wh": 57,  "weight_kg": 1.12 } },
                "lp2": { "brand": "Apple",     "model": "MacBook Pro 16",      "price": 3499, "in_stock": true,  "memory_id": "mem5", "display_id": "dsp4", "tags": ["creative","professional","macos"],   "specs": { "cpu": { "brand": "Apple", "cores": 12, "ghz": 4.05}, "battery_wh": 100, "weight_kg": 2.15 } },
                "lp3": { "brand": "Asus",      "model": "ROG Zephyrus G14",    "price": 1699, "in_stock": true,  "memory_id": "mem3", "display_id": "dsp5", "tags": ["gaming","amd","portable"],           "specs": { "cpu": { "brand": "AMD",   "cores": 8,  "ghz": 4.9 }, "battery_wh": 76,  "weight_kg": 1.65 } },
                "lp4": { "brand": "Dell",      "model": "XPS 15",              "price": 1899, "in_stock": false, "memory_id": "mem3", "display_id": "dsp4", "tags": ["creative","windows","4k"],           "specs": { "cpu": { "brand": "Intel", "cores": 14, "ghz": 3.8 }, "battery_wh": 86,  "weight_kg": 1.86 } },
                "lp5": { "brand": "Razer",     "model": "Blade 15",            "price": 2499, "in_stock": true,  "memory_id": "mem4", "display_id": "dsp3", "tags": ["gaming","windows","rgb"],            "specs": { "cpu": { "brand": "Intel", "cores": 14, "ghz": 4.1 }, "battery_wh": 80,  "weight_kg": 2.01 } },
                "lp6": { "brand": "Framework", "model": "Laptop 13",           "price": 849,  "in_stock": true,  "memory_id": "mem1", "display_id": "dsp1", "tags": ["modular","linux","budget"],          "specs": { "cpu": { "brand": "Intel", "cores": 10, "ghz": 3.3 }, "battery_wh": 55,  "weight_kg": 1.3  } }
            }
        }),
    );
}

fn get(db: &engine::Db, payload: Value) -> Value {
    handlers::process_get(db, &payload, TEST_MAX_BODY, TEST_MAX_KEYS).1
}
fn set(db: &engine::Db, payload: Value) -> Value {
    handlers::process_set(db, &payload, TEST_MAX_BODY, TEST_MAX_KEYS).1
}
fn update(db: &engine::Db, payload: Value) -> Value {
    handlers::process_update(db, &payload, TEST_MAX_BODY, TEST_MAX_KEYS).1
}
fn delete(db: &engine::Db, payload: Value) -> Value {
    handlers::process_delete(db, &payload, TEST_MAX_BODY, TEST_MAX_KEYS).1
}

fn arr(v: &Value) -> &Vec<Value> {
    v.as_array().expect("expected array result")
}

// ─── §1-3: Seed / basic set ───────────────────────────────────────────────────

#[test]
fn test_set_returns_count() {
    println!("[TEST] test_set_returns_count");
    let db = open_db();
    let r = set(
        &db,
        json!({
            "collection": "memory",
            "data": { "mem1": { "capacity_gb": 8 }, "mem2": { "capacity_gb": 16 } }
        }),
    );
    assert_eq!(r["count"], 2);
    assert_eq!(r["status"], "ok");
}

#[test]
fn test_set_array_format_auto_keys() {
    println!("[TEST] test_set_array_format_auto_keys");
    let db = open_db();
    let r = set(
        &db,
        json!({
            "collection": "items",
            "data": [{ "name": "a" }, { "name": "b" }, { "name": "c" }]
        }),
    );
    assert_eq!(r["count"], 3);
    assert_eq!(r["status"], "ok");
    let all = get(&db, json!({ "collection": "items" }));
    assert_eq!(arr(&all).len(), 3);
}

// ─── §4-6: Basic reads ────────────────────────────────────────────────────────

#[test]
fn test_get_single_key() {
    println!("[TEST] test_get_single_key");
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp2" }));
    assert_eq!(r["brand"], "Apple");
    assert_eq!(r["model"], "MacBook Pro 16");
}

#[test]
fn test_get_all() {
    println!("[TEST] test_get_all");
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({ "collection": "laptops" }));
    assert_eq!(arr(&r).len(), 6);
}

#[test]
fn test_get_batch_keys() {
    println!("[TEST] test_get_batch_keys");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({ "collection": "laptops", "keys": ["lp1","lp3","lp5"] }),
    );
    assert_eq!(arr(&r).len(), 3);
}

#[test]
fn test_get_missing_key_returns_error() {
    println!("[TEST] test_get_missing_key_returns_error");
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp99" }));
    assert!(r.get("error").is_some());
}

// ─── §7-10: Field selection ───────────────────────────────────────────────────

#[test]
fn test_fields_projection() {
    println!("[TEST] test_fields_projection");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand", "model", "price"]
        }),
    );
    let first = &arr(&r)[0];
    assert!(first.get("brand").is_some());
    assert!(first.get("price").is_some());
    assert!(first.get("in_stock").is_none());
}

#[test]
fn test_nested_field_projection() {
    println!("[TEST] test_nested_field_projection");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand", "specs.cpu.ghz", "specs.cpu.cores"]
        }),
    );
    let first = &arr(&r)[0];
    assert!(first["specs"]["cpu"].get("ghz").is_some());
    assert!(first["specs"]["cpu"].get("brand").is_none());
}

#[test]
fn test_excluded_fields() {
    println!("[TEST] test_excluded_fields");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "excludedFields": ["price", "memory_id", "display_id"]
        }),
    );
    let first = &arr(&r)[0];
    assert!(first.get("price").is_none());
    assert!(first.get("brand").is_some());
}

#[test]
fn test_fields_and_excluded_fields_error() {
    println!("[TEST] test_fields_and_excluded_fields_error");
    let db = open_db();
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand"],
            "excludedFields": ["price"]
        }),
    );
    assert!(r.get("error").is_some());
}

// ─── §11-20: WHERE clause ─────────────────────────────────────────────────────

#[test]
fn test_where_exact_match() {
    println!("[TEST] test_where_exact_match");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "brand": "Apple" }
        }),
    );
    let results = arr(&r);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["brand"], "Apple");
}

#[test]
fn test_where_numeric_range() {
    println!("[TEST] test_where_numeric_range");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "price": { "$gt": 1000, "$lt": 2000 } }
        }),
    );
    let results = arr(&r);
    assert_eq!(results.len(), 3); // lp1(1499), lp3(1699), lp4(1899)
    for doc in results {
        let p = doc["price"].as_f64().unwrap();
        assert!(p > 1000.0 && p < 2000.0);
    }
}

#[test]
fn test_where_nested_field() {
    println!("[TEST] test_where_nested_field");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "specs.cpu.cores": { "$gte": 12 } }
        }),
    );
    // lp1(12), lp2(12), lp4(14), lp5(14) = 4
    assert_eq!(arr(&r).len(), 4);
}

#[test]
fn test_where_ne() {
    println!("[TEST] test_where_ne");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "specs.cpu.brand": { "$ne": "Intel" } }
        }),
    );
    // Apple + AMD = 2
    assert_eq!(arr(&r).len(), 2);
}

#[test]
fn test_where_contains_string() {
    println!("[TEST] test_where_contains_string");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "model": { "$contains": "Pro" } }
        }),
    );
    assert_eq!(arr(&r).len(), 1);
    assert_eq!(arr(&r)[0]["brand"], "Apple");
}

#[test]
fn test_where_contains_array() {
    println!("[TEST] test_where_contains_array");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "tags": { "$contains": "gaming" } }
        }),
    );
    assert_eq!(arr(&r).len(), 2); // lp3, lp5
}

#[test]
fn test_where_in() {
    println!("[TEST] test_where_in");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "brand": { "$in": ["Apple", "Dell", "Razer"] } }
        }),
    );
    assert_eq!(arr(&r).len(), 3);
}

#[test]
fn test_where_nin() {
    println!("[TEST] test_where_nin");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "brand": { "$nin": ["Framework"] } }
        }),
    );
    assert_eq!(arr(&r).len(), 5);
}

#[test]
fn test_where_combined() {
    println!("[TEST] test_where_combined");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "in_stock": true, "tags": { "$contains": "gaming" }, "price": { "$lt": 2000 } }
        }),
    );
    // lp3 (gaming, in_stock, 1699)
    assert_eq!(arr(&r).len(), 1);
}

// ─── §21-25: Sort ─────────────────────────────────────────────────────────────

#[test]
fn test_sort_price_asc() {
    println!("[TEST] test_sort_price_asc");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand", "price"],
            "sort": [{ "field": "price", "order": "asc" }]
        }),
    );
    let prices: Vec<f64> = arr(&r)
        .iter()
        .map(|d| d["price"].as_f64().unwrap())
        .collect();
    assert_eq!(prices, vec![849.0, 1499.0, 1699.0, 1899.0, 2499.0, 3499.0]);
}

#[test]
fn test_sort_price_desc() {
    println!("[TEST] test_sort_price_desc");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand", "price"],
            "sort": [{ "field": "price", "order": "desc" }]
        }),
    );
    let prices: Vec<f64> = arr(&r)
        .iter()
        .map(|d| d["price"].as_f64().unwrap())
        .collect();
    assert_eq!(prices, vec![3499.0, 2499.0, 1899.0, 1699.0, 1499.0, 849.0]);
}

#[test]
fn test_sort_nested_field() {
    println!("[TEST] test_sort_nested_field");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "sort": [{ "field": "specs.cpu.cores", "order": "desc" }]
        }),
    );
    let first_cores = arr(&r)[0]["specs"]["cpu"]["cores"].as_f64().unwrap();
    assert_eq!(first_cores, 14.0);
}

#[test]
fn test_sort_multi_field() {
    println!("[TEST] test_sort_multi_field");
    let db = open_db();
    seed(&db);
    // Inject a second Apple laptop with a lower price to force the secondary sort tier
    set(
        &db,
        json!({
            "collection": "laptops",
            "data": {
                "lp_apple2": { "brand": "Apple", "model": "MacBook Air 15", "price": 1299, "in_stock": true }
            }
        }),
    );
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand", "price"],
            "sort": [{ "field": "brand", "order": "asc" }, { "field": "price", "order": "asc" }]
        }),
    );
    let results = arr(&r);
    // First two docs must both be Apple, cheaper one first (secondary sort by price asc)
    assert_eq!(results[0]["brand"], "Apple");
    assert_eq!(results[1]["brand"], "Apple");
    assert!(results[0]["price"].as_f64().unwrap() < results[1]["price"].as_f64().unwrap());
}

// ─── §26-28: Pagination ───────────────────────────────────────────────────────

#[test]
fn test_count_limit() {
    println!("[TEST] test_count_limit");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "sort": [{ "field": "price", "order": "asc" }],
            "count": 3
        }),
    );
    assert_eq!(arr(&r).len(), 3);
    assert_eq!(arr(&r)[0]["price"], 849);
}

#[test]
fn test_offset_and_count() {
    println!("[TEST] test_offset_and_count");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "sort": [{ "field": "price", "order": "asc" }],
            "offset": 2,
            "count": 2
        }),
    );
    let results = arr(&r);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["price"], 1699); // lp3
    assert_eq!(results[1]["price"], 1899); // lp4
}

#[test]
fn test_offset_count_with_where() {
    println!("[TEST] test_offset_count_with_where");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "in_stock": true },
            "sort": [{ "field": "price", "order": "asc" }],
            "offset": 2,
            "count": 2
        }),
    );
    assert_eq!(arr(&r).len(), 2);
}

// ─── §29-36: Joins ────────────────────────────────────────────────────────────

#[test]
fn test_join_memory() {
    println!("[TEST] test_join_memory");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand", "model"],
            "joins": [{ "ram": { "from": "memory", "on": "memory_id" } }]
        }),
    );
    let first = &arr(&r)[0];
    assert!(first.get("ram").is_some());
    assert!(first["ram"].get("capacity_gb").is_some());
}

#[test]
fn test_join_with_field_projection() {
    println!("[TEST] test_join_with_field_projection");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand", "model"],
            "joins": [{ "screen": { "from": "display", "on": "display_id", "fields": ["refresh_hz", "panel"] } }]
        }),
    );
    let first = &arr(&r)[0];
    assert!(first["screen"].get("refresh_hz").is_some());
    assert!(first["screen"].get("size_inch").is_none());
}

#[test]
fn test_double_join() {
    println!("[TEST] test_double_join");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand"],
            "joins": [
                { "ram":    { "from": "memory",  "on": "memory_id",  "fields": ["capacity_gb"] } },
                { "screen": { "from": "display", "on": "display_id", "fields": ["panel"] } }
            ]
        }),
    );
    let first = &arr(&r)[0];
    assert!(first.get("ram").is_some());
    assert!(first.get("screen").is_some());
}

#[test]
fn test_join_where_on_joined_field() {
    println!("[TEST] test_join_where_on_joined_field");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand", "model"],
            "joins": [{ "screen": { "from": "display", "on": "display_id", "fields": ["panel"] } }],
            "where": { "screen.panel": { "$in": ["OLED", "Mini-LED"] } }
        }),
    );
    // lp2 (dsp4=Mini-LED), lp1 (dsp2=OLED), lp4 (dsp4=Mini-LED) = 3
    assert_eq!(arr(&r).len(), 3);
}

#[test]
fn test_join_where_upgradeable_ram() {
    println!("[TEST] test_join_where_upgradeable_ram");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "joins": [{ "ram": { "from": "memory", "on": "memory_id", "fields": ["upgradeable"] } }],
            "where": { "ram.upgradeable": true }
        }),
    );
    // mem3 and mem4 are upgradeable → lp3, lp4, lp5
    assert_eq!(arr(&r).len(), 3);
}

#[test]
fn test_join_sort_on_joined_field() {
    println!("[TEST] test_join_sort_on_joined_field");
    let db = open_db();
    seed(&db);
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "fields": ["brand"],
            "joins": [{ "screen": { "from": "display", "on": "display_id", "fields": ["refresh_hz"] } }],
            "sort": [{ "field": "screen.refresh_hz", "order": "desc" }]
        }),
    );
    let first_hz = arr(&r)[0]["screen"]["refresh_hz"].as_f64().unwrap();
    assert_eq!(first_hz, 165.0); // dsp5
}

// ─── §37-41: Update & Delete ──────────────────────────────────────────────────

#[test]
fn test_update_single() {
    println!("[TEST] test_update_single");
    let db = open_db();
    seed(&db);
    update(
        &db,
        json!({
            "collection": "laptops",
            "data": { "lp4": { "in_stock": true, "price": 1749 } }
        }),
    );
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp4" }));
    assert_eq!(r["in_stock"], true);
    assert_eq!(r["price"], 1749);
    assert_eq!(r["brand"], "Dell");
}

#[test]
fn test_update_multiple() {
    println!("[TEST] test_update_multiple");
    let db = open_db();
    seed(&db);
    update(
        &db,
        json!({
            "collection": "laptops",
            "data": { "lp1": { "price": 1399 }, "lp6": { "price": 799 } }
        }),
    );
    let r1 = get(&db, json!({ "collection": "laptops", "keys": "lp1" }));
    let r6 = get(&db, json!({ "collection": "laptops", "keys": "lp6" }));
    assert_eq!(r1["price"], 1399);
    assert_eq!(r6["price"], 799);
}

#[test]
fn test_delete_single() {
    println!("[TEST] test_delete_single");
    let db = open_db();
    seed(&db);
    let r = delete(&db, json!({ "collection": "laptops", "keys": "lp6" }));
    assert_eq!(r["status"], "ok");
    let check = get(&db, json!({ "collection": "laptops", "keys": "lp6" }));
    assert!(check.get("error").is_some());
}

#[test]
fn test_delete_batch() {
    println!("[TEST] test_delete_batch");
    let db = open_db();
    seed(&db);
    delete(
        &db,
        json!({ "collection": "laptops", "keys": ["lp4", "lp5"] }),
    );
    let all = get(&db, json!({ "collection": "laptops" }));
    assert_eq!(arr(&all).len(), 4);
}

#[test]
fn test_drop_collection() {
    println!("[TEST] test_drop_collection");
    let db = open_db();
    seed(&db);
    let r = delete(&db, json!({ "collection": "laptops", "drop": true }));
    assert_eq!(r["dropped"], true);
    let all = get(&db, json!({ "collection": "laptops" }));
    assert!(all.get("error").is_some());
}

// ─── §42-44: Bulk delete with where ──────────────────────────────────────────

#[test]
fn test_bulk_delete_with_where() {
    println!("[TEST] test_bulk_delete_with_where");
    let db = open_db();
    seed(&db);
    let r = delete(
        &db,
        json!({
            "collection": "laptops",
            "where": { "in_stock": false }
        }),
    );
    assert_eq!(r["status"], "ok");
    let all = get(&db, json!({ "collection": "laptops" }));
    for doc in arr(&all) {
        assert_eq!(doc["in_stock"], true);
    }
}

#[test]
fn test_bulk_delete_with_count() {
    println!("[TEST] test_bulk_delete_with_count");
    let db = open_db();
    seed(&db);
    let r = delete(
        &db,
        json!({
            "collection": "laptops",
            "where": { "tags": { "$contains": "gaming" } },
            "count": 1
        }),
    );
    assert_eq!(r["status"], "ok");
    // Only 1 of the 2 gaming laptops removed
    let remaining = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "tags": { "$contains": "gaming" } }
        }),
    );
    assert_eq!(arr(&remaining).len(), 1);
}

// Insert four matching docs one at a time so their `_seq` order is deterministic
// (o1 oldest ... o4 newest), then assert `count` + `order` remove the right ones.
fn seed_ordered(db: &engine::Db) {
    for key in ["o1", "o2", "o3", "o4"] {
        set(
            db,
            json!({
                "collection": "ordered",
                "data": { key: { "tag": "x" } }
            }),
        );
    }
}

#[test]
fn test_bulk_delete_default_order_removes_oldest() {
    println!("[TEST] test_bulk_delete_default_order_removes_oldest");
    let db = open_db();
    seed_ordered(&db);
    // No `order` given → default is oldest-first (lowest _seq). count=2 removes o1, o2.
    let r = delete(
        &db,
        json!({
            "collection": "ordered",
            "where": { "tag": "x" },
            "count": 2
        }),
    );
    assert_eq!(r["status"], "ok");
    assert_eq!(r["deleted"], 2);
    // o1 and o2 (oldest) are gone; o3 and o4 remain.
    let gone = get(&db, json!({ "collection": "ordered", "keys": ["o1", "o2"] }));
    assert!(gone.get("error").is_some());
    let remaining = get(&db, json!({ "collection": "ordered", "keys": ["o3", "o4"] }));
    assert_eq!(arr(&remaining).len(), 2);
}

#[test]
fn test_bulk_delete_desc_order_removes_newest() {
    println!("[TEST] test_bulk_delete_desc_order_removes_newest");
    let db = open_db();
    seed_ordered(&db);
    // order="desc" → newest-first (highest _seq). count=2 removes o4, o3.
    let r = delete(
        &db,
        json!({
            "collection": "ordered",
            "where": { "tag": "x" },
            "count": 2,
            "order": "desc"
        }),
    );
    assert_eq!(r["status"], "ok");
    assert_eq!(r["deleted"], 2);
    // o3 and o4 (newest) are gone; o1 and o2 remain.
    let gone = get(&db, json!({ "collection": "ordered", "keys": ["o3", "o4"] }));
    assert!(gone.get("error").is_some());
    let remaining = get(&db, json!({ "collection": "ordered", "keys": ["o1", "o2"] }));
    assert_eq!(arr(&remaining).len(), 2);
}

// ─── §45-47: Versioning ───────────────────────────────────────────────────────

#[test]
fn test_versioning_fields_present() {
    println!("[TEST] test_versioning_fields_present");
    let db = open_db();
    seed(&db);
    // _v is always returned; _createdAt/_modifiedAt are opt-in via fields projection
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp1" }));
    assert!(r.get("_v").is_some());
    assert_eq!(r["_v"], 1);
    // Request opt-in system fields explicitly
    let r2 = get(
        &db,
        json!({
            "collection": "laptops",
            "keys": "lp1",
            "fields": ["brand", "_createdAt", "_modifiedAt"]
        }),
    );
    assert!(r2.get("_createdAt").is_some());
    assert!(r2.get("_modifiedAt").is_some());
}

#[test]
fn test_versioning_increments_on_update() {
    println!("[TEST] test_versioning_increments_on_update");
    let db = open_db();
    seed(&db);
    update(
        &db,
        json!({
            "collection": "laptops",
            "data": { "lp1": { "price": 1299 } }
        }),
    );
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp1" }));
    assert_eq!(r["_v"], 2);
}

#[test]
fn test_stale_version_write_skipped() {
    println!("[TEST] test_stale_version_write_skipped");
    let db = open_db();
    seed(&db);
    update(
        &db,
        json!({
            "collection": "laptops",
            "data": { "lp4": { "price": 1749 } }
        }),
    );
    // Try to overwrite with stale _v:1 — should be skipped
    set(
        &db,
        json!({
            "collection": "laptops",
            "data": { "lp4": { "brand": "Dell", "model": "XPS 15 STALE", "price": 999, "_v": 1 } }
        }),
    );
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp4" }));
    assert_ne!(r["model"], "XPS 15 STALE");
    assert_eq!(r["price"], 1749);
}

// ─── §50-53: Extends ─────────────────────────────────────────────────────────

#[test]
fn test_extends_embeds_reference() {
    println!("[TEST] test_extends_embeds_reference");
    let db = open_db();
    seed(&db);
    set(
        &db,
        json!({
            "collection": "laptops",
            "data": {
                "lp7": {
                    "brand": "MSI", "model": "Titan GT77", "price": 3299,
                    "extends": { "ram": "memory.mem4", "screen": "display.dsp3" }
                }
            }
        }),
    );
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp7" }));
    assert!(r.get("ram").is_some());
    assert!(r.get("screen").is_some());
    assert_eq!(r["ram"]["capacity_gb"], 64);
    assert!(r.get("extends").is_none());
}

#[test]
fn test_extends_missing_reference_succeeds() {
    println!("[TEST] test_extends_missing_reference_succeeds");
    let db = open_db();
    seed(&db);
    let r = set(
        &db,
        json!({
            "collection": "laptops",
            "data": {
                "lp8": {
                    "brand": "Lenovo", "model": "Legion 5", "price": 1199,
                    "extends": { "ram": "memory.mem99" }
                }
            }
        }),
    );
    assert_eq!(r["status"], "ok");
    let doc = get(&db, json!({ "collection": "laptops", "keys": "lp8" }));
    assert!(doc.get("ram").is_none());
    assert_eq!(doc["brand"], "Lenovo");
}

// ─── §54-57: Validation ───────────────────────────────────────────────────────

#[test]
fn test_invalid_collection_name_path_traversal() {
    println!("[TEST] test_invalid_collection_name_path_traversal");
    let db = open_db();
    let r = set(
        &db,
        json!({
            "collection": "../etc/passwd",
            "data": { "test": { "value": "hack" } }
        }),
    );
    assert!(r.get("error").is_some());
}

#[test]
fn test_reserved_collection_name() {
    println!("[TEST] test_reserved_collection_name");
    let db = open_db();
    let r = set(
        &db,
        json!({
            "collection": "admin",
            "data": { "test": { "value": "data" } }
        }),
    );
    assert!(r.get("error").is_some());
}

#[test]
fn test_unknown_property_set() {
    println!("[TEST] test_unknown_property_set");
    let db = open_db();
    let r = set(
        &db,
        json!({
            "collection": "laptops",
            "wrongProperty": "test",
            "data": { "lp9": { "brand": "X" } }
        }),
    );
    assert!(r.get("error").is_some());
}

#[test]
fn test_unknown_property_get() {
    println!("[TEST] test_unknown_property_get");
    let db = open_db();
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "myInvalidProperty": "test",
            "keys": "lp1"
        }),
    );
    assert!(r.get("error").is_some());
}

// ─── Persistence: data survives reopen ───────────────────────────────────────

#[test]
fn test_persistence_survives_reopen() {
    println!("[TEST] test_persistence_survives_reopen");
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!("target/test_persist_{}.log", id);
    let _ = std::fs::remove_file(&path);
    {
        let db = engine::Db::open(engine::DbConfig {
            path: path.clone(),
            sync_mode: true,
            rate_limit_requests: None,
            rate_limit_window: None,
            max_body_size: TEST_MAX_BODY,
            max_keys_per_request: TEST_MAX_KEYS,
            encryption_key: None,
            in_memory: false,
        })
        .unwrap();
        set(
            &db,
            json!({
                "collection": "items",
                "data": { "k1": { "value": 42 } }
            }),
        );
    }
    let db2 = engine::Db::open(engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        rate_limit_requests: None,
        rate_limit_window: None,
        max_body_size: TEST_MAX_BODY,
        max_keys_per_request: TEST_MAX_KEYS,
        encryption_key: None,
        in_memory: false,
    })
    .unwrap();
    let r = get(&db2, json!({ "collection": "items", "keys": "k1" }));
    assert_eq!(r["value"], 42);
    let _ = std::fs::remove_file(&path);
}

// ─── Compaction ───────────────────────────────────────────────────────────────

#[test]
fn test_compaction_preserves_data() {
    println!("[TEST] test_compaction_preserves_data");
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!("target/test_compact_{}.log", id);
    let _ = std::fs::remove_file(&path);
    let db = engine::Db::open(engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        rate_limit_requests: None,
        rate_limit_window: None,
        max_body_size: TEST_MAX_BODY,
        max_keys_per_request: TEST_MAX_KEYS,
        encryption_key: None,
        in_memory: false,
    })
    .unwrap();
    seed(&db);
    delete(&db, json!({ "collection": "laptops", "keys": "lp6" }));
    db.compact().expect("compact");
    let db2 = engine::Db::open(engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        rate_limit_requests: None,
        rate_limit_window: None,
        max_body_size: TEST_MAX_BODY,
        max_keys_per_request: TEST_MAX_KEYS,
        encryption_key: None,
        in_memory: false,
    })
    .unwrap();
    let all = get(&db2, json!({ "collection": "laptops" }));
    assert_eq!(arr(&all).len(), 5);
    let r = get(&db2, json!({ "collection": "laptops", "keys": "lp2" }));
    assert_eq!(r["brand"], "Apple");
    let _ = std::fs::remove_file(&path);
}

// ─── Concurrent writes ────────────────────────────────────────────────────────

#[test]
fn test_concurrent_writes() {
    println!("[TEST] test_concurrent_writes");
    use std::thread;
    let db = Arc::new(open_db());
    let n_threads: usize = 8;
    let n_docs: usize = 10;
    let mut handles = vec![];
    for t in 0..n_threads {
        let db = db.clone();
        handles.push(thread::spawn(move || {
            for i in 0..n_docs {
                let key = format!("doc_{}_{}", t, i);
                set(
                    &db,
                    json!({
                        "collection": "stress",
                        "data": { key: { "thread": t, "index": i } }
                    }),
                );
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let all = get(&db, json!({ "collection": "stress" }));
    assert_eq!(arr(&all).len(), n_threads * n_docs);
}

#[test]
fn test_concurrent_reads_during_writes() {
    println!("[TEST] test_concurrent_reads_during_writes");
    use std::thread;
    let db = Arc::new(open_db());
    for i in 0..50 {
        set(
            &db,
            json!({
                "collection": "rw",
                "data": { format!("k{}", i): { "v": i } }
            }),
        );
    }
    let db_w = db.clone();
    let db_r = db.clone();
    let writer = thread::spawn(move || {
        for i in 50..150 {
            set(
                &db_w,
                json!({
                    "collection": "rw",
                    "data": { format!("k{}", i): { "v": i } }
                }),
            );
        }
    });
    let reader = thread::spawn(move || {
        for _ in 0..100 {
            let _ = get(&db_r, json!({ "collection": "rw" }));
        }
    });
    writer.join().unwrap();
    reader.join().unwrap();
}

// ─── maxSize (capped collections) ────────────────────────────────────────────

#[test]
fn test_max_size_evicts_oldest() {
    println!("[TEST] test_max_size_evicts_oldest");
    let db = open_db();
    // Register capped collection with max 3 documents
    handlers::process_schema(
        &db,
        &json!({
            "collection": "recent_events",
            "maxSize": 3
        }),
        TEST_MAX_BODY,
        TEST_MAX_KEYS,
    );
    // Insert 5 documents — oldest 2 should be evicted
    set(
        &db,
        json!({
            "collection": "recent_events",
            "data": {
                "evt_001": { "type": "login" },
                "evt_002": { "type": "view" },
                "evt_003": { "type": "logout" }
            }
        }),
    );
    set(
        &db,
        json!({
            "collection": "recent_events",
            "data": {
                "evt_004": { "type": "login" },
                "evt_005": { "type": "purchase" }
            }
        }),
    );
    let all = get(&db, json!({ "collection": "recent_events" }));
    assert_eq!(arr(&all).len(), 3);
    // Oldest (evt_001, evt_002) should be gone
    let r1 = get(
        &db,
        json!({ "collection": "recent_events", "keys": "evt_001" }),
    );
    assert!(r1.get("error").is_some());
}

// ─── where + count pagination correctness ────────────────────────────────────

#[test]
fn test_where_count_returns_correct_ordered_subset() {
    println!("[TEST] test_where_count_returns_correct_ordered_subset");
    let db = open_db();
    seed(&db);
    // Get first 2 in-stock laptops by insertion order (no sort)
    let r = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "in_stock": true },
            "count": 2
        }),
    );
    assert_eq!(arr(&r).len(), 2);
}

#[test]
fn test_where_offset_count_pagination() {
    println!("[TEST] test_where_offset_count_pagination");
    let db = open_db();
    seed(&db);
    let page1 = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "in_stock": true },
            "sort": [{ "field": "price", "order": "asc" }],
            "count": 2,
            "offset": 0
        }),
    );
    let page2 = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "in_stock": true },
            "sort": [{ "field": "price", "order": "asc" }],
            "count": 2,
            "offset": 2
        }),
    );
    let p1 = arr(&page1);
    let p2 = arr(&page2);
    assert_eq!(p1.len(), 2);
    assert_eq!(p2.len(), 2);
    // Pages must not overlap
    let p1_prices: Vec<f64> = p1.iter().map(|d| d["price"].as_f64().unwrap()).collect();
    let p2_prices: Vec<f64> = p2.iter().map(|d| d["price"].as_f64().unwrap()).collect();
    assert!(p1_prices[1] <= p2_prices[0]);
}

// ─── Concurrency: same-key overwrites ───────────────────────────────────────

#[test]
fn test_concurrent_same_key_overwrites() {
    println!("[TEST] test_concurrent_same_key_overwrites");
    use std::thread;
    let db = Arc::new(open_db());
    let n_threads = 8;
    let n_iterations = 50;
    let target_key = "global_hot_ticker";

    let mut handles = vec![];
    for t in 0..n_threads {
        let db = db.clone();
        handles.push(thread::spawn(move || {
            for i in 0..n_iterations {
                set(
                    &db,
                    json!({
                        "collection": "race",
                        "data": { target_key: { "last_writer": t, "counter": i } }
                    }),
                );
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Database must remain stable and the key fully retrievable
    let final_doc = get(&db, json!({ "collection": "race", "keys": target_key }));
    assert!(final_doc.get("counter").is_some());
    assert!(final_doc.get("error").is_none());
}

// ─── Where: schema-less type adversarial resilience ──────────────────────────

#[test]
fn test_where_schema_less_type_adversarial_resilience() {
    println!("[TEST] test_where_schema_less_type_adversarial_resilience");
    let db = open_db();
    // Mixed types in the same field across documents
    set(
        &db,
        json!({
            "collection": "chaos",
            "data": {
                "c1": { "metric": 10 },
                "c2": { "metric": "Not An Integer" },
                "c3": { "metric": [10, 20] },
                "c4": { "metric": 50 },
                "c5": {}
            }
        }),
    );

    let r = get(
        &db,
        json!({
            "collection": "chaos",
            "where": { "metric": { "$gt": 5 } }
        }),
    );

    // Engine must skip non-numeric / missing fields and return only c1 and c4
    let results = arr(&r);
    assert_eq!(results.len(), 2);
}

// ─── Validation: empty operator arrays ───────────────────────────────────────

#[test]
fn test_empty_operators_graceful_handling() {
    println!("[TEST] test_empty_operators_graceful_handling");
    let db = open_db();
    seed(&db);

    // Empty $in → no documents match; engine returns a "not found" error response
    let r_in = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "brand": { "$in": [] } }
        }),
    );
    assert!(r_in.get("error").is_some());

    // Empty $nin → all documents match
    let r_nin = get(
        &db,
        json!({
            "collection": "laptops",
            "where": { "brand": { "$nin": [] } }
        }),
    );
    assert_eq!(arr(&r_nin).len(), 6);
}
