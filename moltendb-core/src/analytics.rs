#![allow(dead_code)]
// ─── analytics.rs ─────────────────────────────────────────────────────────────
// This file implements the analytics query engine used by the WASM dashboard.
//
// What is an "analytics query"?
//   A regular database query returns raw documents (rows of data).
//   An analytics query returns a single computed number — a metric — derived
//   from many documents. Examples:
//     - "How many events happened in the last minute?" → COUNT
//     - "What is the average order value?" → AVG
//     - "What is the highest score?" → MAX
//
// How it's used:
//   The analytics dashboard (analytics_dashboard.html) calls:
//     client.startAutoRefresh(queries, 500)
//   Every 500ms, the worker calls:
//     db.analytics(JSON.stringify(query))
//   which calls execute_query() here and returns a JSON string with the result.
//
// Data flow:
//   analytics_dashboard.html
//     → analytics-worker.js (db.analytics)
//       → worker.rs (WorkerDb::analytics)
//         → analytics::execute_query()
//           → db.get_all() → filter → compute metric → return AnalyticsResult
// ─────────────────────────────────────────────────────────────────────────────

// Deserialize = automatically parse JSON into this struct.
// Serialize   = automatically convert this struct to JSON.
use serde::{Deserialize, Serialize};
// Value = a generic JSON value (can be string, number, object, array, null).
// json! = a macro that creates a Value from a JSON literal.
use serde_json::{Value, json};
// Db = the main database handle (holds in-memory state + storage backend).
// query = the query module with helper functions like evaluate_where and get_nested_value.
use crate::{engine::Db, engine::DbError, query};

// ─── AnalyticsQuery ───────────────────────────────────────────────────────────

/// The full analytics query sent from the dashboard.
///
/// Example JSON (what the dashboard sends):
/// ```json
/// {
///   "collection": "events",
///   "metric": { "type": "COUNT" },
///   "where": { "event_type": "button_click" }
/// }
/// ```
///
/// The `#[derive(Debug, Deserialize)]` annotation tells Rust to:
///   - `Debug`: allow printing this struct with `{:?}` for debugging.
///   - `Deserialize`: automatically parse it from JSON using serde.
#[derive(Debug, Deserialize)]
pub struct AnalyticsQuery {
    /// The name of the collection to query (e.g. "events", "users").
    pub collection: String,

    /// Which metric to compute (COUNT, SUM, AVG, MIN, MAX).
    pub metric: MetricSpec,

    /// Optional filter — only documents matching this WHERE clause are included.
    /// The `#[serde(rename = "where")]` tells serde to look for the JSON key
    /// "where" (not "filter") when deserializing. "where" is a reserved keyword
    /// in Rust, so we can't use it as a field name directly.
    #[serde(rename = "where")]
    pub filter: Option<Value>,
}

// ─── MetricSpec ───────────────────────────────────────────────────────────────

/// Which metric to compute and (for aggregations) which field to aggregate over.
///
/// This is a "tagged enum" — serde uses the "type" JSON key to decide which
/// variant to deserialize into. For example:
///   `{ "type": "COUNT" }` → MetricSpec::Count
///   `{ "type": "SUM", "field": "price" }` → MetricSpec::Sum { field: "price" }
///
/// `#[serde(tag = "type")]` tells serde to use the "type" field as the discriminant.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum MetricSpec {
    /// Count the number of matching documents. No field needed.
    /// JSON: `{ "type": "COUNT" }`
    #[serde(rename = "COUNT")]
    Count,

    /// Sum the numeric values of a field across all matching documents.
    /// JSON: `{ "type": "SUM", "field": "price" }`
    #[serde(rename = "SUM")]
    Sum { field: String },

    /// Compute the arithmetic mean (average) of a field.
    /// JSON: `{ "type": "AVG", "field": "score" }`
    #[serde(rename = "AVG")]
    Avg { field: String },

    /// Find the smallest value of a field across all matching documents.
    /// JSON: `{ "type": "MIN", "field": "age" }`
    #[serde(rename = "MIN")]
    Min { field: String },

    /// Find the largest value of a field across all matching documents.
    /// JSON: `{ "type": "MAX", "field": "age" }`
    #[serde(rename = "MAX")]
    Max { field: String },
}

// ─── AnalyticsResult ──────────────────────────────────────────────────────────

/// The result returned to the dashboard after executing an analytics query.
///
/// `result` is the computed metric value (a number, or null if no data).
/// `metadata` contains performance information shown in the dashboard UI.
///
/// `#[derive(Serialize)]` lets this struct be converted to JSON automatically.
#[derive(Debug, Serialize)]
pub struct AnalyticsResult {
    /// The computed metric value.
    /// For COUNT: a JSON integer (e.g. 42).
    /// For SUM/AVG/MIN/MAX: a JSON float (e.g. 3.14), or null if no documents matched.
    pub result: Value,

    /// Performance metadata — how long the query took and how many rows were scanned.
    pub metadata: ResultMetadata,
}

/// Performance metadata attached to every analytics result.
///
/// The dashboard uses `execution_time_ms` to display the "Avg Query Latency" counter.
/// `rows_scanned` is useful for understanding query cost (before indexes kick in).
#[derive(Debug, Serialize)]
pub struct ResultMetadata {
    /// Wall-clock time from query start to result ready, in milliseconds.
    /// Measured using `web_time::Instant` which works in both native and WASM.
    pub execution_time_ms: u64,

    /// Number of documents that passed the WHERE filter and were included in
    /// the metric computation. For COUNT this equals the result; for others
    /// it's the number of documents whose field value was extracted.
    pub rows_scanned: usize,
}

// ─── execute_query ────────────────────────────────────────────────────────────

/// Execute an analytics query against the database and return the result.
///
/// Steps:
///   1. Record the start time (for execution_time_ms).
///   2. Fetch all documents from the requested collection (O(n) copy).
///   3. Filter documents using the WHERE clause (if provided).
///   4. Compute the requested metric over the filtered documents.
///   5. Return the result with timing metadata.
///
/// # Arguments
/// * `db`    — The database handle. Used to call `db.get_all(collection)`.
/// * `query` — The parsed analytics query (collection + metric + optional filter).
pub fn execute_query(db: &Db, query: &AnalyticsQuery) -> Result<AnalyticsResult, DbError> {
    // Record the start time. `web_time::Instant` is used instead of
    // `std::time::Instant` because `std::time` is not available in WASM.
    let start = web_time::Instant::now();

    // Fetch all documents in the collection as a HashMap<String, Value>.
    // This is an O(n) operation — it copies every document out of the DashMap.
    // For large collections, this is the main performance bottleneck.
    let all_docs = db.get_all(&query.collection);

    // Apply the WHERE filter (if any) to narrow down the documents.
    // `filter()` is a lazy iterator adapter — it doesn't allocate until `.collect()`.
    // `evaluate_where(doc, filter)` returns true if the document matches all conditions.
    let mut filtered_docs = Vec::new();
    for doc in all_docs.values() {
        if let Some(filter) = &query.filter {
            if query::evaluate_where(doc, filter)? {
                filtered_docs.push(doc);
            }
        } else {
            filtered_docs.push(doc);
        }
    }

    // Record how many documents passed the filter (used in metadata).
    let rows_scanned = filtered_docs.len();

    // Compute the requested metric over the filtered documents.
    // Each arm of the match returns a serde_json::Value (a JSON number or null).
    let result = match &query.metric {
        // COUNT: just return the number of matching documents.
        MetricSpec::Count => {
            json!(filtered_docs.len())
        }

        // SUM: extract the numeric field from each document and add them up.
        // `filter_map` skips documents where the field is missing or non-numeric.
        // `.sum()` works on f64 iterators — returns 0.0 if the iterator is empty.
        MetricSpec::Sum { field } => {
            let sum: f64 = filtered_docs
                .iter()
                .filter_map(|doc| extract_number(doc, field))
                .sum();
            json!(sum)
        }

        // AVG: collect all numeric values, then divide sum by count.
        // Returns null (Value::Null) if no documents have the field.
        MetricSpec::Avg { field } => {
            let values: Vec<f64> = filtered_docs
                .iter()
                .filter_map(|doc| extract_number(doc, field))
                .collect();

            if values.is_empty() {
                // No numeric values found — return null instead of NaN or 0.
                Value::Null
            } else {
                // `.iter().sum::<f64>()` sums the Vec<f64>.
                // `values.len() as f64` converts usize to f64 for division.
                let avg = values.iter().sum::<f64>() / values.len() as f64;
                json!(avg)
            }
        }

        // MIN: find the smallest numeric value.
        // `min_by` requires a comparator because f64 doesn't implement Ord
        // (due to NaN — NaN is not comparable). `partial_cmp` handles this
        // by returning None for NaN, and `unwrap()` is safe here because
        // extract_number() never returns NaN.
        MetricSpec::Min { field } => {
            let min = filtered_docs
                .iter()
                .filter_map(|doc| extract_number(doc, field))
                .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            // `map(|v| json!(v))` converts Some(f64) to Some(Value::Number).
            // `.unwrap_or(Value::Null)` returns null if no documents had the field.
            min.map(|v| json!(v)).unwrap_or(Value::Null)
        }

        // MAX: find the largest numeric value. Same logic as MIN but reversed.
        MetricSpec::Max { field } => {
            let max = filtered_docs
                .iter()
                .filter_map(|doc| extract_number(doc, field))
                .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            max.map(|v| json!(v)).unwrap_or(Value::Null)
        }
    };

    // Measure how long the query took from start to now.
    // `.as_millis()` returns u128; we cast to u64 (safe for any realistic query time).
    let execution_time_ms = start.elapsed().as_millis() as u64;

    Ok(AnalyticsResult {
        result,
        metadata: ResultMetadata {
            execution_time_ms,
            rows_scanned,
        },
    })
}

// ─── extract_number ───────────────────────────────────────────────────────────

/// Extract a numeric value from a document, supporting nested dot-notation paths.
///
/// Examples:
///   `extract_number(&doc, "price")`         → reads doc["price"]
///   `extract_number(&doc, "meta.discount")` → reads doc["meta"]["discount"]
///
/// Returns `None` if:
///   - The field doesn't exist in the document.
///   - The field exists but is not a number (e.g. it's a string or null).
///
/// Tries three numeric types in order:
///   1. f64 (floating point) — covers most JSON numbers.
///   2. i64 (signed integer) — for integers that don't fit in f64 exactly.
///   3. u64 (unsigned integer) — for very large positive integers.
fn extract_number(doc: &Value, field: &str) -> Option<f64> {
    // Split "meta.discount" into ["meta", "discount"] for nested traversal.
    let parts: Vec<&str> = field.split('.').collect();

    // `get_nested_value` traverses the document following the path parts.
    // Returns None if any part of the path doesn't exist.
    let value = query::get_nested_value(doc, &parts)?;

    // Try to interpret the JSON value as a number.
    // `or_else` chains fallbacks — if as_f64() returns None, try as_i64(), etc.
    value.as_f64()
        .or_else(|| value.as_i64().map(|i| i as f64))
        .or_else(|| value.as_u64().map(|u| u as f64))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Verify that extract_number correctly reads top-level and nested numeric fields,
    /// and returns None for missing fields.
    #[test]
    fn test_extract_number() {
        let doc = json!({
            "price": 99.99,
            "quantity": 5,
            "meta": {
                "discount": 10.5
            }
        });

        // Top-level float field.
        assert_eq!(extract_number(&doc, "price"), Some(99.99));
        // Top-level integer field — returned as f64.
        assert_eq!(extract_number(&doc, "quantity"), Some(5.0));
        // Nested field via dot notation.
        assert_eq!(extract_number(&doc, "meta.discount"), Some(10.5));
        // Missing field — should return None, not panic.
        assert_eq!(extract_number(&doc, "nonexistent"), None);
    }
}
