/// MoltenDB integration test suite
/// Tests all handler operations using an in-memory SyncDiskStorage backed by a temp file.
use moltendb::{engine, handlers};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ─── Helpers ──────────────────────────────────────────────────────────────────

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Open a fresh in-memory database backed by a unique temp file.
fn open_db() -> engine::Db {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!("target/test_db_{}.log", id);
    let _ = std::fs::remove_file(&path);
    let db_config = engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        rate_limit_requests: 100,
        rate_limit_window: 60,
        max_body_size: 10485760,
        max_keys_per_request: 1000,
        encryption_key: None,
        post_backup_script: None,
    };
    engine::Db::open(db_config).expect("open db")
}

/// Seed the three standard collections used by most tests.
fn seed(db: &engine::Db) {
    handlers::process_set(db, &json!({
        "collection": "memory",
        "data": {
            "mem1": { "capacity_gb": 8,  "type": "LPDDR5", "speed_mhz": 4266, "upgradeable": false },
            "mem2": { "capacity_gb": 16, "type": "LPDDR5", "speed_mhz": 4266, "upgradeable": false },
            "mem3": { "capacity_gb": 32, "type": "DDR5",   "speed_mhz": 5600, "upgradeable": true  },
            "mem4": { "capacity_gb": 64, "type": "DDR5",   "speed_mhz": 5600, "upgradeable": true  },
            "mem5": { "capacity_gb": 36, "type": "Unified","speed_mhz": 6400, "upgradeable": false }
        }
    }), TEST_MAX_BODY, TEST_MAX_KEYS);
    handlers::process_set(db, &json!({
        "collection": "display",
        "data": {
            "dsp1": { "size_inch": 13.3, "resolution": "2560x1600", "panel": "IPS",      "refresh_hz": 60,  "hdr": false },
            "dsp2": { "size_inch": 14.0, "resolution": "2880x1800", "panel": "OLED",     "refresh_hz": 90,  "hdr": true  },
            "dsp3": { "size_inch": 15.6, "resolution": "1920x1080", "panel": "IPS",      "refresh_hz": 144, "hdr": false },
            "dsp4": { "size_inch": 16.2, "resolution": "3456x2234", "panel": "Mini-LED", "refresh_hz": 120, "hdr": true  },
            "dsp5": { "size_inch": 14.0, "resolution": "2560x1600", "panel": "IPS",      "refresh_hz": 165, "hdr": false }
        }
    }), TEST_MAX_BODY, TEST_MAX_KEYS);
    handlers::process_set(db, &json!({
        "collection": "laptops",
        "data": {
            "lp1": { "brand": "Lenovo",    "model": "ThinkPad X1 Carbon", "price": 1499, "in_stock": true,  "memory_id": "mem2", "display_id": "dsp2", "tags": ["business","ultrabook","lightweight"], "specs": { "cpu": { "brand": "Intel", "cores": 12, "ghz": 3.5 }, "battery_wh": 57,  "weight_kg": 1.12 } },
            "lp2": { "brand": "Apple",     "model": "MacBook Pro 16",      "price": 3499, "in_stock": true,  "memory_id": "mem5", "display_id": "dsp4", "tags": ["creative","professional","macos"],   "specs": { "cpu": { "brand": "Apple", "cores": 12, "ghz": 4.05}, "battery_wh": 100, "weight_kg": 2.15 } },
            "lp3": { "brand": "Asus",      "model": "ROG Zephyrus G14",    "price": 1699, "in_stock": true,  "memory_id": "mem3", "display_id": "dsp5", "tags": ["gaming","amd","portable"],           "specs": { "cpu": { "brand": "AMD",   "cores": 8,  "ghz": 4.9 }, "battery_wh": 76,  "weight_kg": 1.65 } },
            "lp4": { "brand": "Dell",      "model": "XPS 15",              "price": 1899, "in_stock": false, "memory_id": "mem3", "display_id": "dsp4", "tags": ["creative","windows","4k"],           "specs": { "cpu": { "brand": "Intel", "cores": 14, "ghz": 3.8 }, "battery_wh": 86,  "weight_kg": 1.86 } },
            "lp5": { "brand": "Razer",     "model": "Blade 15",            "price": 2499, "in_stock": true,  "memory_id": "mem4", "display_id": "dsp3", "tags": ["gaming","windows","rgb"],            "specs": { "cpu": { "brand": "Intel", "cores": 14, "ghz": 4.1 }, "battery_wh": 80,  "weight_kg": 2.01 } },
            "lp6": { "brand": "Framework", "model": "Laptop 13",           "price": 849,  "in_stock": true,  "memory_id": "mem1", "display_id": "dsp1", "tags": ["modular","linux","budget"],          "specs": { "cpu": { "brand": "Intel", "cores": 10, "ghz": 3.3 }, "battery_wh": 55,  "weight_kg": 1.3  } }
        }
    }), TEST_MAX_BODY, TEST_MAX_KEYS);
}

const TEST_MAX_BODY: usize = 10 * 1024 * 1024;
const TEST_MAX_KEYS: usize = 1000;

fn body(r: (u16, Value)) -> Value { r.1 }
fn status(r: &(u16, Value)) -> u16 { r.0 }

fn get(db: &engine::Db, payload: serde_json::Value) -> Value {
    handlers::process_get(db, &payload, TEST_MAX_BODY, TEST_MAX_KEYS).1
}
fn set(db: &engine::Db, payload: serde_json::Value) -> Value {
    handlers::process_set(db, &payload, TEST_MAX_BODY, TEST_MAX_KEYS).1
}
fn update(db: &engine::Db, payload: serde_json::Value) -> Value {
    handlers::process_update(db, &payload, TEST_MAX_BODY, TEST_MAX_KEYS).1
}
fn delete(db: &engine::Db, payload: serde_json::Value) -> Value {
    handlers::process_delete(db, &payload, TEST_MAX_BODY, TEST_MAX_KEYS).1
}

fn arr(v: &Value) -> &Vec<Value> {
    v.as_array().expect("expected array result")
}

// ─── §1-3: Seed / basic set ───────────────────────────────────────────────────

#[test]
fn test_set_returns_count() {
    let db = open_db();
    let r = set(&db, json!({
        "collection": "memory",
        "data": { "mem1": { "capacity_gb": 8 }, "mem2": { "capacity_gb": 16 } }
    }));
    assert_eq!(r["count"], 2);
    assert_eq!(r["status"], "ok");
}

#[test]
fn test_set_array_format_auto_keys() {
    let db = open_db();
    let r = set(&db, json!({
        "collection": "items",
        "data": [{ "name": "a" }, { "name": "b" }, { "name": "c" }]
    }));
    assert_eq!(r["count"], 3);
    assert_eq!(r["status"], "ok");
    let all = get(&db, json!({ "collection": "items" }));
    assert_eq!(arr(&all).len(), 3);
}

// ─── §4-6: Basic reads ────────────────────────────────────────────────────────

#[test]
fn test_get_single_key() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp2" }));
    assert_eq!(r["brand"], "Apple");
    assert_eq!(r["model"], "MacBook Pro 16");
}

#[test]
fn test_get_all() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({ "collection": "laptops" }));
    assert_eq!(arr(&r).len(), 6);
}

#[test]
fn test_get_batch_keys() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({ "collection": "laptops", "keys": ["lp1","lp3","lp5"] }));
    assert_eq!(arr(&r).len(), 3);
}

#[test]
fn test_get_missing_key_returns_null() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp99" }));
    assert!(r.get("error").is_some());
}

// ─── §7-10: Field selection ───────────────────────────────────────────────────

#[test]
fn test_fields_projection() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand", "model", "price"]
    }));
    let first = &arr(&r)[0];
    assert!(first.get("brand").is_some());
    assert!(first.get("price").is_some());
    assert!(first.get("in_stock").is_none());
}

#[test]
fn test_nested_field_projection() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand", "specs.cpu.ghz", "specs.cpu.cores"]
    }));
    let first = &arr(&r)[0];
    assert!(first["specs"]["cpu"].get("ghz").is_some());
    assert!(first["specs"]["cpu"].get("brand").is_none());
}

#[test]
fn test_excluded_fields() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "excludedFields": ["price", "memory_id", "display_id"]
    }));
    let first = &arr(&r)[0];
    assert!(first.get("price").is_none());
    assert!(first.get("brand").is_some());
}

#[test]
fn test_fields_and_excluded_fields_error() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand"],
        "excludedFields": ["price"]
    }));
    assert!(r.get("error").is_some());
}

// ─── §11-20: WHERE clause ─────────────────────────────────────────────────────

#[test]
fn test_where_exact_match() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "brand": "Apple" }
    }));
    let results = arr(&r);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["brand"], "Apple");
}

#[test]
fn test_where_numeric_range() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "price": { "$gt": 1000, "$lt": 2000 } }
    }));
    let results = arr(&r);
    assert_eq!(results.len(), 3); // lp1(1499), lp3(1699), lp4(1899)
    for doc in results {
        let p = doc["price"].as_f64().unwrap();
        assert!(p > 1000.0 && p < 2000.0);
    }
}

#[test]
fn test_where_nested_field() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "specs.cpu.cores": { "$gte": 12 } }
    }));
    // lp1(12), lp2(12), lp4(14), lp5(14) = 4
    assert_eq!(arr(&r).len(), 4);
}

#[test]
fn test_where_ne() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "specs.cpu.brand": { "$ne": "Intel" } }
    }));
    // Apple + AMD = 2
    assert_eq!(arr(&r).len(), 2);
}

#[test]
fn test_where_contains_string() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "model": { "$contains": "Pro" } }
    }));
    assert_eq!(arr(&r).len(), 1);
    assert_eq!(arr(&r)[0]["brand"], "Apple");
}

#[test]
fn test_where_contains_array() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "tags": { "$contains": "gaming" } }
    }));
    assert_eq!(arr(&r).len(), 2); // lp3, lp5
}

#[test]
fn test_where_in() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "brand": { "$in": ["Apple", "Dell", "Razer"] } }
    }));
    assert_eq!(arr(&r).len(), 3);
}

#[test]
fn test_where_nin() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "brand": { "$nin": ["Framework"] } }
    }));
    assert_eq!(arr(&r).len(), 5);
}

#[test]
fn test_where_combined() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "in_stock": true, "tags": { "$contains": "gaming" }, "price": { "$lt": 2000 } }
    }));
    // lp3 (gaming, in_stock, 1699)
    assert_eq!(arr(&r).len(), 1);
}

// ─── §21-25: Sort ─────────────────────────────────────────────────────────────

#[test]
fn test_sort_price_asc() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand", "price"],
        "sort": [{ "field": "price", "order": "asc" }]
    }));
    let prices: Vec<f64> = arr(&r).iter().map(|d| d["price"].as_f64().unwrap()).collect();
    assert_eq!(prices, vec![849.0, 1499.0, 1699.0, 1899.0, 2499.0, 3499.0]);
}

#[test]
fn test_sort_price_desc() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand", "price"],
        "sort": [{ "field": "price", "order": "desc" }]
    }));
    let prices: Vec<f64> = arr(&r).iter().map(|d| d["price"].as_f64().unwrap()).collect();
    assert_eq!(prices, vec![3499.0, 2499.0, 1899.0, 1699.0, 1499.0, 849.0]);
}

#[test]
fn test_sort_nested_field() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "sort": [{ "field": "specs.cpu.cores", "order": "desc" }]
    }));
    let first_cores = arr(&r)[0]["specs"]["cpu"]["cores"].as_f64().unwrap();
    assert_eq!(first_cores, 14.0);
}

#[test]
fn test_sort_multi_field() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand", "price"],
        "sort": [{ "field": "brand", "order": "asc" }, { "field": "price", "order": "asc" }]
    }));
    // First brand alphabetically is Apple
    assert_eq!(arr(&r)[0]["brand"], "Apple");
}

// ─── §26-28: Pagination ───────────────────────────────────────────────────────

#[test]
fn test_count_limit() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "sort": [{ "field": "price", "order": "asc" }],
        "count": 3
    }));
    assert_eq!(arr(&r).len(), 3);
    assert_eq!(arr(&r)[0]["price"], 849);
}

#[test]
fn test_offset_and_count() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "sort": [{ "field": "price", "order": "asc" }],
        "offset": 2,
        "count": 2
    }));
    let results = arr(&r);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["price"], 1699); // lp3
    assert_eq!(results[1]["price"], 1899); // lp4
}

#[test]
fn test_offset_count_with_where() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "in_stock": true },
        "sort": [{ "field": "price", "order": "asc" }],
        "offset": 2,
        "count": 2
    }));
    assert_eq!(arr(&r).len(), 2);
}

// ─── §29-36: Joins ────────────────────────────────────────────────────────────

#[test]
fn test_join_memory() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand", "model"],
        "joins": [{ "ram": { "from": "memory", "on": "memory_id" } }]
    }));
    let first = &arr(&r)[0];
    assert!(first.get("ram").is_some());
    assert!(first["ram"].get("capacity_gb").is_some());
}

#[test]
fn test_join_with_field_projection() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand", "model"],
        "joins": [{ "screen": { "from": "display", "on": "display_id", "fields": ["refresh_hz", "panel"] } }]
    }));
    let first = &arr(&r)[0];
    assert!(first["screen"].get("refresh_hz").is_some());
    assert!(first["screen"].get("size_inch").is_none());
}

#[test]
fn test_double_join() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand"],
        "joins": [
            { "ram":    { "from": "memory",  "on": "memory_id",  "fields": ["capacity_gb"] } },
            { "screen": { "from": "display", "on": "display_id", "fields": ["panel"] } }
        ]
    }));
    let first = &arr(&r)[0];
    assert!(first.get("ram").is_some());
    assert!(first.get("screen").is_some());
}

#[test]
fn test_join_where_on_joined_field() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand", "model"],
        "joins": [{ "screen": { "from": "display", "on": "display_id", "fields": ["panel"] } }],
        "where": { "screen.panel": { "$in": ["OLED", "Mini-LED"] } }
    }));
    // lp2 (dsp4=Mini-LED), lp1 (dsp2=OLED), lp4 (dsp4=Mini-LED) = 3
    assert_eq!(arr(&r).len(), 3);
}

#[test]
fn test_join_where_upgradeable_ram() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "joins": [{ "ram": { "from": "memory", "on": "memory_id", "fields": ["upgradeable"] } }],
        "where": { "ram.upgradeable": true }
    }));
    // mem3 and mem4 are upgradeable → lp3, lp4, lp5
    assert_eq!(arr(&r).len(), 3);
}

#[test]
fn test_join_sort_on_joined_field() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({
        "collection": "laptops",
        "fields": ["brand"],
        "joins": [{ "screen": { "from": "display", "on": "display_id", "fields": ["refresh_hz"] } }],
        "sort": [{ "field": "screen.refresh_hz", "order": "desc" }]
    }));
    let first_hz = arr(&r)[0]["screen"]["refresh_hz"].as_f64().unwrap();
    assert_eq!(first_hz, 165.0); // dsp5
}

// ─── §37-41: Update & Delete ──────────────────────────────────────────────────

#[test]
fn test_update_single() {
    let db = open_db();
    seed(&db);
    update(&db, json!({
        "collection": "laptops",
        "data": { "lp4": { "in_stock": true, "price": 1749 } }
    }));
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp4" }));
    assert_eq!(r["in_stock"], true);
    assert_eq!(r["price"], 1749);
    // Original fields preserved
    assert_eq!(r["brand"], "Dell");
}

#[test]
fn test_update_multiple() {
    let db = open_db();
    seed(&db);
    update(&db, json!({
        "collection": "laptops",
        "data": { "lp1": { "price": 1399 }, "lp6": { "price": 799 } }
    }));
    let r1 = get(&db, json!({ "collection": "laptops", "keys": "lp1" }));
    let r6 = get(&db, json!({ "collection": "laptops", "keys": "lp6" }));
    assert_eq!(r1["price"], 1399);
    assert_eq!(r6["price"], 799);
}

#[test]
fn test_delete_single() {
    let db = open_db();
    seed(&db);
    let r = delete(&db, json!({ "collection": "laptops", "keys": "lp6" }));
    assert_eq!(r["status"], "ok");
    let check = get(&db, json!({ "collection": "laptops", "keys": "lp6" }));
    assert!(check.get("error").is_some());
}

#[test]
fn test_delete_batch() {
    let db = open_db();
    seed(&db);
    delete(&db, json!({ "collection": "laptops", "keys": ["lp4", "lp5"] }));
    let all = get(&db, json!({ "collection": "laptops" }));
    assert_eq!(arr(&all).len(), 4);
}

#[test]
fn test_drop_collection() {
    let db = open_db();
    seed(&db);
    let r = delete(&db, json!({ "collection": "laptops", "drop": true }));
    assert_eq!(r["dropped"], true);
    let all = get(&db, json!({ "collection": "laptops" }));
    assert!(all.get("error").is_some());
}

// ─── §45-46: Versioning ───────────────────────────────────────────────────────

#[test]
fn test_versioning_fields_present() {
    let db = open_db();
    seed(&db);
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp1" }));
    assert!(r.get("_v").is_some());
    assert!(r.get("createdAt").is_some());
    assert!(r.get("modifiedAt").is_some());
    assert_eq!(r["_v"], 1);
}

#[test]
fn test_versioning_increments_on_update() {
    let db = open_db();
    seed(&db);
    update(&db, json!({
        "collection": "laptops",
        "data": { "lp1": { "price": 1299 } }
    }));
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp1" }));
    assert_eq!(r["_v"], 2);
}

#[test]
fn test_stale_version_write_skipped() {
    let db = open_db();
    seed(&db);
    // Update lp4 to bump _v to 2
    update(&db, json!({
        "collection": "laptops",
        "data": { "lp4": { "price": 1749 } }
    }));
    // Try to overwrite with stale _v:1 — should be skipped
    set(&db, json!({
        "collection": "laptops",
        "data": { "lp4": { "brand": "Dell", "model": "XPS 15 STALE", "price": 999, "_v": 1 } }
    }));
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp4" }));
    assert_ne!(r["model"], "XPS 15 STALE");
    assert_eq!(r["price"], 1749);
}

// ─── §50-53: Extends ─────────────────────────────────────────────────────────

#[test]
fn test_extends_embeds_reference() {
    let db = open_db();
    seed(&db);
    set(&db, json!({
        "collection": "laptops",
        "data": {
            "lp7": {
                "brand": "MSI", "model": "Titan GT77", "price": 3299,
                "extends": { "ram": "memory.mem4", "screen": "display.dsp3" }
            }
        }
    }));
    let r = get(&db, json!({ "collection": "laptops", "keys": "lp7" }));
    assert!(r.get("ram").is_some());
    assert!(r.get("screen").is_some());
    assert_eq!(r["ram"]["capacity_gb"], 64);
    assert!(r.get("extends").is_none()); // extends key consumed
}

#[test]
fn test_extends_missing_reference_succeeds() {
    let db = open_db();
    seed(&db);
    let r = set(&db, json!({
        "collection": "laptops",
        "data": {
            "lp8": {
                "brand": "Lenovo", "model": "Legion 5", "price": 1199,
                "extends": { "ram": "memory.mem99" }
            }
        }
    }));
    assert_eq!(r["status"], "ok");
    let doc = get(&db, json!({ "collection": "laptops", "keys": "lp8" }));
    assert!(doc.get("ram").is_none()); // missing ref → field not added
    assert_eq!(doc["brand"], "Lenovo");
}

// ─── §54-57: Validation ───────────────────────────────────────────────────────

#[test]
fn test_invalid_collection_name_path_traversal() {
    let db = open_db();
    let r = set(&db, json!({
        "collection": "../etc/passwd",
        "data": { "test": { "value": "hack" } }
    }));
    assert!(r.get("error").is_some());
}

#[test]
fn test_reserved_collection_name() {
    let db = open_db();
    let r = set(&db, json!({
        "collection": "admin",
        "data": { "test": { "value": "data" } }
    }));
    assert!(r.get("error").is_some());
}

#[test]
fn test_unknown_property_set() {
    let db = open_db();
    let r = set(&db, json!({
        "collection": "laptops",
        "wrongProperty": "test",
        "data": { "lp9": { "brand": "X" } }
    }));
    assert!(r.get("error").is_some());
}

#[test]
fn test_unknown_property_get() {
    let db = open_db();
    let r = get(&db, json!({
        "collection": "laptops",
        "myInvalidProperty": "test",
        "keys": "lp1"
    }));
    assert!(r.get("error").is_some());
}

// ─── Persistence: data survives reopen ───────────────────────────────────────

#[test]
fn test_persistence_survives_reopen() {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!("target/test_persist_{}.log", id);
    let _ = std::fs::remove_file(&path);
    {
        let db_config = engine::DbConfig {
            path: path.clone(),
            sync_mode: true,
            hot_threshold: 50000,
            rate_limit_requests: Some(100),
            rate_limit_window: Some(60),
            max_body_size: 10485760,
            encryption_key: None,
            post_backup_script: None,
        };
        let db = engine::Db::open(db_config).unwrap();
        set(&db, json!({
            "collection": "items",
            "data": { "k1": { "value": 42 } }
        }));
    }
    // Reopen and verify data is still there
    let db_config2 = engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        hot_threshold: 50000,
        rate_limit_requests: 100,
        rate_limit_window: 60,
        max_body_size: 10485760,
        max_keys_per_request: 1000,
        encryption_key: None,
        post_backup_script: None,
    };
    let db2 = engine::Db::open(db_config2).unwrap();
    let r = get(&db2, json!({ "collection": "items", "keys": "k1" }));
    assert_eq!(r["value"], 42);
    let _ = std::fs::remove_file(&path);
}

// ─── Compaction ───────────────────────────────────────────────────────────────

#[test]
fn test_compaction_preserves_data() {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!("target/test_compact_{}.log", id);
    let _ = std::fs::remove_file(&path);
    let db_config = engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        hot_threshold: 50000,
        rate_limit_requests: 100,
        rate_limit_window: 60,
        max_body_size: 10485760,
        max_keys_per_request: 1000,
        encryption_key: None,
        post_backup_script: None,
    };
    let db = engine::Db::open(db_config).unwrap();
    seed(&db);
    delete(&db, json!({ "collection": "laptops", "keys": "lp6" }));
    db.compact().expect("compact");
    // Reopen after compaction
    let db_config2 = engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        hot_threshold: 50000,
        rate_limit_requests: 100,
        rate_limit_window: 60,
        max_body_size: 10485760,
        max_keys_per_request: 1000,
        encryption_key: None,
        post_backup_script: None,
    };
    let db2 = engine::Db::open(db_config2).unwrap();
    let all = get(&db2, json!({ "collection": "laptops" }));
    assert_eq!(arr(&all).len(), 5); // lp6 deleted
    let r = get(&db2, json!({ "collection": "laptops", "keys": "lp2" }));
    assert_eq!(r["brand"], "Apple");
    let _ = std::fs::remove_file(&path);
}

// ─── Stress: concurrent writes ────────────────────────────────────────────────

#[test]
fn test_concurrent_writes() {
    use std::thread;
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!("target/test_concurrent_{}.log", id);
    let _ = std::fs::remove_file(&path);
    let db_config = engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        hot_threshold: 50000,
        rate_limit_requests: 100,
        rate_limit_window: 60,
        max_body_size: 10485760,
        max_keys_per_request: 1000,
        encryption_key: None,
        post_backup_script: None,
    };
    let db = Arc::new(engine::Db::open(db_config).unwrap());
    let n_threads = 8;
    let n_docs = 100;
    let mut handles = vec![];
    for t in 0..n_threads {
        let db = db.clone();
        handles.push(thread::spawn(move || {
            for i in 0..n_docs {
                let key = format!("doc_{}_{}", t, i);
                set(&db, json!({
                    "collection": "stress",
                    "data": { key: { "thread": t, "index": i, "value": t * n_docs + i } }
                }));
            }
        }));
    }
    for h in handles { h.join().unwrap(); }
    let all = get(&db, json!({ "collection": "stress" }));
    assert_eq!(arr(&all).len(), n_threads * n_docs);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_concurrent_reads_during_writes() {
    use std::thread;
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!("target/test_rw_{}.log", id);
    let _ = std::fs::remove_file(&path);
    let db_config = engine::DbConfig {
        path: path.clone(),
        sync_mode: true,
        hot_threshold: 50000,
        rate_limit_requests: 100,
        rate_limit_window: 60,
        max_body_size: 10485760,
        max_keys_per_request: 1000,
        encryption_key: None,
        post_backup_script: None,
    };
    let db = Arc::new(engine::Db::open(db_config).unwrap());
    // Pre-seed
    for i in 0..50 {
        set(&db, json!({
            "collection": "rw",
            "data": { format!("k{}", i): { "v": i } }
        }));
    }
    let db_w = db.clone();
    let db_r = db.clone();
    let writer = thread::spawn(move || {
        for i in 50..150 {
            handlers::process_set(&db_w, &json!({
                "collection": "rw",
                "data": { format!("k{}", i): { "v": i } }
            }), TEST_MAX_BODY, TEST_MAX_KEYS);
        }
    });
    let reader = thread::spawn(move || {
        for _ in 0..100 {
            let _ = handlers::process_get(&db_r, &json!({ "collection": "rw" }), TEST_MAX_BODY, TEST_MAX_KEYS);
        }
    });
    writer.join().unwrap();
    reader.join().unwrap();
    let _ = std::fs::remove_file(&path);
}

// ─── Index auto-creation ──────────────────────────────────────────────────────

#[test]
fn test_index_accelerated_query() {
    let db = open_db();
    seed(&db);
    // Query brand 3 times to trigger auto-index creation
    for _ in 0..3 {
        get(&db, json!({
            "collection": "laptops",
            "where": { "brand": "Apple" }
        }));
    }
    // Index should now exist
    assert!(db.indexes.contains_key("laptops:brand"));
    // Query via index still returns correct results
    let r = get(&db, json!({
        "collection": "laptops",
        "where": { "brand": "Apple" }
    }));
    assert_eq!(arr(&r).len(), 1);
    assert_eq!(arr(&r)[0]["brand"], "Apple");
}

// ─── Analytics ────────────────────────────────────────────────────────────────

#[test]
fn test_analytics_count() {
    use moltendb::analytics::{AnalyticsQuery, execute_query};
    let db = open_db();
    seed(&db);
    let q: AnalyticsQuery = serde_json::from_value(json!({
        "collection": "laptops",
        "metric": { "type": "COUNT" }
    })).unwrap();
    let result = execute_query(&db, &q);
    assert_eq!(result.result, serde_json::json!(6));
}

#[test]
fn test_analytics_sum() {
    use moltendb::analytics::{AnalyticsQuery, execute_query};
    let db = open_db();
    seed(&db);
    let q: AnalyticsQuery = serde_json::from_value(json!({
        "collection": "laptops",
        "metric": { "type": "SUM", "field": "price" }
    })).unwrap();
    let result = execute_query(&db, &q);
    // 1499+3499+1699+1899+2499+849 = 11944
    assert_eq!(result.result, serde_json::json!(11944.0));
}

#[test]
fn test_analytics_avg() {
    use moltendb::analytics::{AnalyticsQuery, execute_query};
    let db = open_db();
    seed(&db);
    let q: AnalyticsQuery = serde_json::from_value(json!({
        "collection": "laptops",
        "metric": { "type": "AVG", "field": "price" }
    })).unwrap();
    let result = execute_query(&db, &q);
    let avg = result.result.as_f64().unwrap();
    assert!((avg - 11944.0 / 6.0).abs() < 0.01);
}

#[test]
fn test_analytics_min_max() {
    use moltendb::analytics::{AnalyticsQuery, execute_query};
    let db = open_db();
    seed(&db);
    let min_q: AnalyticsQuery = serde_json::from_value(json!({
        "collection": "laptops",
        "metric": { "type": "MIN", "field": "price" }
    })).unwrap();
    let max_q: AnalyticsQuery = serde_json::from_value(json!({
        "collection": "laptops",
        "metric": { "type": "MAX", "field": "price" }
    })).unwrap();
    assert_eq!(execute_query(&db, &min_q).result, json!(849.0));
    assert_eq!(execute_query(&db, &max_q).result, json!(3499.0));
}

#[test]
fn test_analytics_with_where() {
    use moltendb::analytics::{AnalyticsQuery, execute_query};
    let db = open_db();
    seed(&db);
    let q: AnalyticsQuery = serde_json::from_value(json!({
        "collection": "laptops",
        "metric": { "type": "COUNT" },
        "where": { "in_stock": true }
    })).unwrap();
    let result = execute_query(&db, &q);
    assert_eq!(result.result, json!(5)); // all except lp4
}

