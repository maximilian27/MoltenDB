use super::*;
use serde_json::json;

#[test]
fn test_evaluate_where_basic() {
    let doc = json!({ "name": "Alice", "age": 30 });
    assert!(evaluate_where(&doc, &json!({ "name": "Alice" })).unwrap());
    assert!(!evaluate_where(&doc, &json!({ "name": "Bob" })).unwrap());
    assert!(evaluate_where(&doc, &json!({ "name": { "$eq": "Alice" } })).unwrap());
    assert!(evaluate_where(&doc, &json!({ "name": "alice" })).unwrap());
}

#[test]
fn test_evaluate_where_numeric() {
    let doc = json!({ "age": 30 });
    assert!(evaluate_where(&doc, &json!({ "age": { "$gt": 20 } })).unwrap());
    assert!(evaluate_where(&doc, &json!({ "age": { "$gte": 30 } })).unwrap());
    assert!(evaluate_where(&doc, &json!({ "age": { "$lt": 40 } })).unwrap());
    assert!(evaluate_where(&doc, &json!({ "age": { "$lte": 30 } })).unwrap());
    assert!(!evaluate_where(&doc, &json!({ "age": { "$gt": 30 } })).unwrap());
}

#[test]
fn test_evaluate_where_invalid_ops() {
    let doc = json!({ "name": "Alice" });
    let res = evaluate_where(&doc, &json!({ "name": { "$invalid": "val" } }));
    assert!(res.is_err());
    if let Err(DbError::InvalidQuery(msg)) = res {
        assert!(msg.contains("Unknown operator"));
    } else {
        panic!("Expected InvalidQuery error");
    }
}

#[test]
fn test_evaluate_where_logical() {
    let doc = json!({ "name": "Alice", "age": 30 });
    assert!(
        evaluate_where(
            &doc,
            &json!({ "$or": [{ "name": "Alice" }, { "name": "Bob" }] })
        )
        .unwrap()
    );
    assert!(evaluate_where(&doc, &json!({ "$or": [{ "name": "Bob" }, { "age": 30 }] })).unwrap());
    assert!(!evaluate_where(&doc, &json!({ "$or": [{ "name": "Bob" }, { "age": 20 }] })).unwrap());
    assert!(
        evaluate_where(
            &doc,
            &json!({ "$and": [{ "name": "Alice" }, { "age": 30 }] })
        )
        .unwrap()
    );
    assert!(
        !evaluate_where(
            &doc,
            &json!({ "$and": [{ "name": "Alice" }, { "age": 20 }] })
        )
        .unwrap()
    );
}

#[test]
fn test_evaluate_where_in_nin() {
    let doc = json!({ "role": "admin" });
    assert!(evaluate_where(&doc, &json!({ "role": { "$in": ["admin", "user"] } })).unwrap());
    assert!(!evaluate_where(&doc, &json!({ "role": { "$in": ["guest", "user"] } })).unwrap());
    assert!(evaluate_where(&doc, &json!({ "role": { "$nin": ["guest", "user"] } })).unwrap());
    assert!(!evaluate_where(&doc, &json!({ "role": { "$nin": ["admin", "user"] } })).unwrap());
}

#[test]
fn test_evaluate_predicate_msgpack_eq_ne() {
    let doc = json!({ "brand": "Apple", "price": 999.0 });
    let bytes = rmp_serde::to_vec(&doc).unwrap();

    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "brand", "$eq", &json!("Apple")),
        Some(true)
    );
    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "brand", "$eq", &json!("apple")),
        Some(true)
    );
    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "brand", "$ne", &json!("Intel")),
        Some(true)
    );
    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "brand", "$ne", &json!("Apple")),
        Some(false)
    );
}

#[test]
fn test_evaluate_predicate_msgpack_in_nin() {
    let doc = json!({ "brand": "Dell" });
    let bytes = rmp_serde::to_vec(&doc).unwrap();

    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "brand", "$in", &json!(["Apple", "Dell", "Razer"])),
        Some(true)
    );
    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "brand", "$in", &json!(["Apple", "Razer"])),
        Some(false)
    );
    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "brand", "$nin", &json!(["Framework", "Lenovo"])),
        Some(true)
    );
    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "brand", "$nin", &json!(["Dell", "Lenovo"])),
        Some(false)
    );
}

#[test]
fn test_evaluate_predicate_msgpack_nested() {
    let doc = json!({ "specs": { "cpu": { "brand": "Intel" } } });
    let bytes = rmp_serde::to_vec(&doc).unwrap();

    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "specs.cpu.brand", "$eq", &json!("Intel")),
        Some(true)
    );
    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "specs.cpu.brand", "$ne", &json!("Intel")),
        Some(false)
    );
    assert_eq!(
        evaluate_predicate_msgpack(&bytes, "specs.cpu.brand", "$nin", &json!(["AMD", "Apple"])),
        Some(true)
    );
}
