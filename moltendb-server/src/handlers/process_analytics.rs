#![allow(dead_code)]
use serde_json::{Value, json};
use crate::validation;
use moltendb_core::{engine, analytics};

/// Handle an analytics query request.
///
/// Parses the payload as an AnalyticsQuery and executes it against the database.
/// Returns the result value and execution metadata (time, rows scanned).
///
/// Note: this function exists but is not currently wired to an HTTP route.
/// It is available for future use or direct calls from other handlers.
pub fn process_analytics(db: &engine::Db, payload: &Value, max_body_size: usize) -> Value {
    // Validate the request structure first.
    if let Err(e) = validation::validate_request(payload, max_body_size) {
        return json!({ "error": e.to_string() });
    }

    // Deserialize the payload into a strongly-typed AnalyticsQuery struct.
    let query: analytics::AnalyticsQuery = match serde_json::from_value(payload.clone()) {
        Ok(q)  => q,
        Err(e) => return json!({ "error": format!("Invalid analytics query: {}", e) }),
    };

    // Execute the analytics query (COUNT, SUM, AVG, etc.) against the database.
    let result = analytics::execute_query(db, &query);

    // Return the result along with execution metadata for performance monitoring.
    json!({
        "result": result.result,
        "metadata": {
            "execution_time_ms": result.metadata.execution_time_ms,
            "rows_scanned": result.metadata.rows_scanned
        }
    })
}