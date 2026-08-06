// Coalesced-scan correctness under concurrency.
//
// Fires many queries at the same collection simultaneously so they fall into the
// same coalescing window and are served by a single shared pass over the
// collection. Both coalesced query shapes are exercised — full-collection WHERE
// scans and single-field numeric-sort top-N scans — including a mix of the two
// in the same window. Verifies every query still receives exactly its own
// correct, independent result set (counts, predicate satisfaction, sort order
// and pagination).

use moltendb_core::engine::{Db, DbConfig};
use moltendb_core::handlers::process_get;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

const N_DOCS: usize = 1000;

fn open_db() -> Db {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("moltendb_coalesce_test_{}.log", id));
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

fn seed(db: &Db) {
    // category cycles A,B,C,D ; value = i ; flag = (i even)
    let cats = ["A", "B", "C", "D"];
    let items: Vec<(String, serde_json::Value)> = (0..N_DOCS)
        .map(|i| {
            (
                format!("item_{:04}", i),
                json!({
                    "category": cats[i % 4],
                    "value": i,
                    "flag": i % 2 == 0,
                }),
            )
        })
        .collect();
    db.insert("stress", items).unwrap();
}

#[test]
fn coalesced_concurrent_where_queries_return_correct_results() {
    let db = open_db();
    seed(&db);

    // 60 concurrent queries — comfortably more than one batch — all launched
    // together so they coalesce into shared passes.
    const N_THREADS: usize = 60;
    let barrier = Arc::new(Barrier::new(N_THREADS));
    let mut handles = Vec::with_capacity(N_THREADS);

    for t in 0..N_THREADS {
        let db = db.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            // Rotate through a few different query shapes so the shared pass has
            // to evaluate distinct predicates against each document.
            barrier.wait();
            match t % 4 {
                // Exact category match: A appears every 4th doc → N_DOCS/4.
                0 => {
                    let payload = json!({
                        "collection": "stress",
                        "where": {"category": "A"},
                        "count": 1000,
                    });
                    let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                    let arr = body.as_array().expect("array");
                    assert_eq!(arr.len(), N_DOCS / 4, "category A count");
                    for d in arr {
                        assert_eq!(d["category"], "A");
                    }
                }
                // Numeric range: value >= 500 → 500 docs.
                1 => {
                    let payload = json!({
                        "collection": "stress",
                        "where": {"value": {"$gte": 500}},
                        "count": 1000,
                    });
                    let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                    let arr = body.as_array().expect("array");
                    assert_eq!(arr.len(), 500, "value>=500 count");
                    for d in arr {
                        assert!(d["value"].as_u64().unwrap() >= 500);
                    }
                }
                // Boolean flag: even values → 500 docs.
                2 => {
                    let payload = json!({
                        "collection": "stress",
                        "where": {"flag": true},
                        "count": 1000,
                    });
                    let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                    let arr = body.as_array().expect("array");
                    assert_eq!(arr.len(), 500, "flag true count");
                    for d in arr {
                        assert_eq!(d["flag"], true);
                        assert_eq!(d["value"].as_u64().unwrap() % 2, 0);
                    }
                }
                // Logical $or (full-evaluator path) + descending order + small count.
                _ => {
                    let payload = json!({
                        "collection": "stress",
                        "where": {"$or": [{"category": "A"}, {"category": "B"}]},
                        "order": "desc",
                        "count": 3,
                    });
                    let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                    let arr = body.as_array().expect("array");
                    assert_eq!(arr.len(), 3, "$or desc count=3");
                    // Highest-seq A/B docs: indices 999(D),998(C),997(B),996(A) →
                    // the top three A/B by insertion order are 997, 996, 993.
                    assert_eq!(arr[0]["value"].as_u64().unwrap(), 997);
                    assert_eq!(arr[1]["value"].as_u64().unwrap(), 996);
                    assert_eq!(arr[2]["value"].as_u64().unwrap(), 993);
                    for d in arr {
                        let c = d["category"].as_str().unwrap();
                        assert!(c == "A" || c == "B");
                    }
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("query thread panicked / assertion failed");
    }
}

#[test]
fn coalesced_offset_pagination_is_correct() {
    let db = open_db();
    seed(&db);

    // Two overlapping paginated queries fired together: they share the pass but
    // must each apply their own offset/count correctly. Default order is desc,
    // so category-A docs are 996, 992, 988, ... (descending by value).
    let barrier = Arc::new(Barrier::new(2));

    let db1 = db.clone();
    let b1 = Arc::clone(&barrier);
    let h1 = std::thread::spawn(move || {
        b1.wait();
        let payload = json!({
            "collection": "stress",
            "where": {"category": "A"},
            "offset": 0,
            "count": 3,
        });
        let (_, body) = process_get(&db1, &payload, 1024 * 1024, 1000);
        let arr = body.as_array().unwrap().clone();
        arr
    });

    let db2 = db.clone();
    let b2 = Arc::clone(&barrier);
    let h2 = std::thread::spawn(move || {
        b2.wait();
        let payload = json!({
            "collection": "stress",
            "where": {"category": "A"},
            "offset": 3,
            "count": 3,
        });
        let (_, body) = process_get(&db2, &payload, 1024 * 1024, 1000);
        let arr = body.as_array().unwrap().clone();
        arr
    });

    let page1 = h1.join().unwrap();
    let page2 = h2.join().unwrap();

    let vals1: Vec<u64> = page1.iter().map(|d| d["value"].as_u64().unwrap()).collect();
    let vals2: Vec<u64> = page2.iter().map(|d| d["value"].as_u64().unwrap()).collect();

    assert_eq!(vals1, vec![996, 992, 988]);
    assert_eq!(vals2, vec![984, 980, 976]);
}

#[test]
fn coalesced_concurrent_sorted_queries_return_correct_results() {
    let db = open_db();
    seed(&db);

    // 60 concurrent single-field numeric-sort queries launched together so they
    // coalesce into shared passes. Each keeps its own bounded heap fed from the
    // one pass, so every query must still get its own correct top-N.
    const N_THREADS: usize = 60;
    let barrier = Arc::new(Barrier::new(N_THREADS));
    let mut handles = Vec::with_capacity(N_THREADS);

    for t in 0..N_THREADS {
        let db = db.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            match t % 4 {
                // Ascending sort on `value` → smallest first: 0,1,2,3,4.
                0 => {
                    let payload = json!({
                        "collection": "stress",
                        "sort": [{"field": "value", "order": "asc"}],
                        "count": 5,
                    });
                    let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                    let arr = body.as_array().expect("array");
                    let vals: Vec<u64> =
                        arr.iter().map(|d| d["value"].as_u64().unwrap()).collect();
                    assert_eq!(vals, vec![0, 1, 2, 3, 4], "asc top-5");
                }
                // Descending sort on `value` → largest first: 999..995.
                1 => {
                    let payload = json!({
                        "collection": "stress",
                        "sort": [{"field": "value", "order": "desc"}],
                        "count": 5,
                    });
                    let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                    let arr = body.as_array().expect("array");
                    let vals: Vec<u64> =
                        arr.iter().map(|d| d["value"].as_u64().unwrap()).collect();
                    assert_eq!(vals, vec![999, 998, 997, 996, 995], "desc top-5");
                }
                // Ascending sort + WHERE category A → A docs are 0,4,8,12,...
                2 => {
                    let payload = json!({
                        "collection": "stress",
                        "where": {"category": "A"},
                        "sort": [{"field": "value", "order": "asc"}],
                        "count": 3,
                    });
                    let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                    let arr = body.as_array().expect("array");
                    let vals: Vec<u64> =
                        arr.iter().map(|d| d["value"].as_u64().unwrap()).collect();
                    assert_eq!(vals, vec![0, 4, 8], "asc top-3 where A");
                    for d in arr {
                        assert_eq!(d["category"], "A");
                    }
                }
                // Descending sort + WHERE value < 500 → 499,498,497.
                _ => {
                    let payload = json!({
                        "collection": "stress",
                        "where": {"value": {"$lt": 500}},
                        "sort": [{"field": "value", "order": "desc"}],
                        "count": 3,
                    });
                    let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                    let arr = body.as_array().expect("array");
                    let vals: Vec<u64> =
                        arr.iter().map(|d| d["value"].as_u64().unwrap()).collect();
                    assert_eq!(vals, vec![499, 498, 497], "desc top-3 where <500");
                    for d in arr {
                        assert!(d["value"].as_u64().unwrap() < 500);
                    }
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("sorted query thread panicked / assertion failed");
    }
}

#[test]
fn coalesced_sorted_offset_pagination_is_correct() {
    let db = open_db();
    seed(&db);

    // Two overlapping paginated sorted queries fired together: they share the
    // pass but must each apply their own offset/count to the shared top-N.
    // Descending on `value`: 999, 998, 997, 996, 995, 994, ...
    let barrier = Arc::new(Barrier::new(2));

    let db1 = db.clone();
    let b1 = Arc::clone(&barrier);
    let h1 = std::thread::spawn(move || {
        b1.wait();
        let payload = json!({
            "collection": "stress",
            "sort": [{"field": "value", "order": "desc"}],
            "offset": 0,
            "count": 3,
        });
        let (_, body) = process_get(&db1, &payload, 1024 * 1024, 1000);
        body.as_array().unwrap().clone()
    });

    let db2 = db.clone();
    let b2 = Arc::clone(&barrier);
    let h2 = std::thread::spawn(move || {
        b2.wait();
        let payload = json!({
            "collection": "stress",
            "sort": [{"field": "value", "order": "desc"}],
            "offset": 3,
            "count": 3,
        });
        let (_, body) = process_get(&db2, &payload, 1024 * 1024, 1000);
        body.as_array().unwrap().clone()
    });

    let page1 = h1.join().unwrap();
    let page2 = h2.join().unwrap();

    let vals1: Vec<u64> = page1.iter().map(|d| d["value"].as_u64().unwrap()).collect();
    let vals2: Vec<u64> = page2.iter().map(|d| d["value"].as_u64().unwrap()).collect();

    assert_eq!(vals1, vec![999, 998, 997]);
    assert_eq!(vals2, vec![996, 995, 994]);
}

#[test]
fn coalesced_mixed_where_and_sorted_share_pass() {
    let db = open_db();
    seed(&db);

    // Fire WHERE scans and sorted top-N scans together in the same window so
    // both request shapes are folded into the same shared pass. Each must still
    // produce its own correct, independent result.
    const N_THREADS: usize = 40;
    let barrier = Arc::new(Barrier::new(N_THREADS));
    let mut handles = Vec::with_capacity(N_THREADS);

    for t in 0..N_THREADS {
        let db = db.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            if t % 2 == 0 {
                // WHERE scan: category A → every 4th doc → 250 matches.
                let payload = json!({
                    "collection": "stress",
                    "where": {"category": "A"},
                    "count": 1000,
                });
                let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                let arr = body.as_array().expect("array");
                assert_eq!(arr.len(), N_DOCS / 4, "category A count");
                for d in arr {
                    assert_eq!(d["category"], "A");
                }
            } else {
                // Sorted top-N: descending on value → 999,998,997.
                let payload = json!({
                    "collection": "stress",
                    "sort": [{"field": "value", "order": "desc"}],
                    "count": 3,
                });
                let (_, body) = process_get(&db, &payload, 1024 * 1024, 1000);
                let arr = body.as_array().expect("array");
                let vals: Vec<u64> = arr.iter().map(|d| d["value"].as_u64().unwrap()).collect();
                assert_eq!(vals, vec![999, 998, 997], "desc top-3");
            }
        }));
    }

    for h in handles {
        h.join().expect("mixed query thread panicked / assertion failed");
    }
}
