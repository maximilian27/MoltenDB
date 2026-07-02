use crate::common::payload_fields::PayloadField;
use crate::common::where_operators::WhereOperator;
use crate::handlers::get::types::FetchParams;
use crate::{engine, query};
use serde_json::Value;

/// Returns true if `op` is supported by `evaluate_predicate_msgpack`
/// (i.e. can be evaluated directly on raw MsgPack bytes without full deserialization).
fn is_fast_path_op(op: &str) -> bool {
    matches!(
        WhereOperator::from_str(op),
        Some(
            WhereOperator::Eq
                | WhereOperator::NotEq
                | WhereOperator::In
                | WhereOperator::NotIn
                | WhereOperator::Gt
                | WhereOperator::Gte
                | WhereOperator::Lt
                | WhereOperator::Lte
                | WhereOperator::Contains
        )
    )
}

/// Which scan strategy `fetch_documents` should use for a full-collection scan
/// (i.e. when no explicit keys were supplied in the request).
///
/// The strategy is chosen once by `choose_scan_strategy` and then dispatched
/// via a `match`, keeping each execution path self-contained and easy to reason
/// about independently.
///
/// Variants (in rough order of selectivity / performance):
///
/// - `WhereOnly`       — WHERE clause present, no joins, no prefix filter.
///                       Uses a parallel scan with atomic early-exit once
///                       `offset + count` matches are found.
///
/// - `PrefixOnly`      — No WHERE clause (or joins present), but a prefix
///                       filter is active. Iterates via `get_filtered` with a
///                       key-prefix predicate; BTreeMap insertion-order is
///                       preserved for correct pagination.
///
/// - `WhereAndPrefix`  — Both a WHERE clause (no joins) and a prefix filter.
///                       Combines both checks in a single `get_filtered` pass
///                       so the collection is only scanned once.
///
/// - `FullScan`        — No WHERE, no prefix, no joins. Passes a trivially-true
///                       predicate to `get_filtered`; the BTreeMap seq_index
///                       gives insertion-order results with early-exit.
enum ScanStrategy {
    /// WHERE only (no joins, no prefix): parallel scan with early-exit.
    WhereOnly,
    /// Prefix filter only (no WHERE or joins present): key-prefix scan.
    PrefixOnly,
    /// WHERE + prefix filter (no joins): single pass combining both checks.
    WhereAndPrefix,
    /// No WHERE, no prefix, no joins: trivial full scan in insertion order.
    FullScan,
}

/// Inspect `params` and decide which `ScanStrategy` to use.
fn choose_scan_strategy(params: &FetchParams<'_>) -> ScanStrategy {
    let has_where = params.where_clause.is_some() && !params.has_joins;
    let has_prefix = params
        .allowed_prefixes
        .map(|p| !p.is_empty())
        .unwrap_or(false);

    match (has_where, has_prefix) {
        (true, false) => ScanStrategy::WhereOnly,
        (false, true) => ScanStrategy::PrefixOnly,
        (true, true) => ScanStrategy::WhereAndPrefix,
        (false, false) => ScanStrategy::FullScan,
    }
}

/// Fetch raw documents from the engine based on the request payload.
///
/// Dispatch table:
/// - Single key string  → `db.get` (point lookup)
/// - Key array          → `db.get` (batch lookup)
/// - No keys            → full-collection scan; the exact scan method is chosen
///                        by `choose_scan_strategy` based on whether a WHERE
///                        clause, a prefix filter, or neither is present.
pub fn fetch_documents(db: &engine::Db, params: &FetchParams<'_>) -> Vec<(String, Value)> {
    let allowed_prefixes_strings: Option<Vec<&str>> = params
        .allowed_prefixes
        .map(|pfx_vec| pfx_vec.iter().filter_map(|v| v.as_str()).collect());

    // ── Key-based lookups ────────────────────────────────────────────────────
    match params.payload.get(PayloadField::Keys.as_str()) {
        Some(Value::String(k)) => {
            if let Some(ref prefixes) = allowed_prefixes_strings {
                if !prefixes.iter().any(|p| k.starts_with(p)) {
                    return Vec::new();
                }
            }
            return db
                .get(params.col_name, vec![k.clone()])
                .into_iter()
                .collect();
        }
        Some(Value::Array(arr)) => {
            let ks = arr
                .iter()
                .filter_map(|v| {
                    let s = v.as_str()?;
                    if let Some(ref prefixes) = allowed_prefixes_strings {
                        if !prefixes.iter().any(|p| s.starts_with(p)) {
                            return None;
                        }
                    }
                    Some(s.to_string())
                })
                .collect();
            return db.get(params.col_name, ks).into_iter().collect();
        }
        _ => {}
    }

    // ── Full-collection scan — strategy selected once, then dispatched ───────
    //
    // count passed to get_filtered:
    //   Some(offset + limit) → BTreeMap / parallel scan can stop early.
    //   None                 → sort present; caller needs all matches first.
    let early_exit_count = if params.has_sort {
        None
    } else {
        Some(params.offset + params.count_limit)
    };

    match choose_scan_strategy(params) {
        // WHERE only: parallel scan with atomic early-exit.
        // The BTreeMap path would degrade to O(N) for sparse matches; Rayon
        // uses all cores and stops softly once `offset + count` matches are
        // collected.
        ScanStrategy::WhereOnly => {
            let clause = params.where_clause.unwrap().clone();
            let fast_preds = extract_fast_predicates(&clause);

            db.get_filtered(
                params.col_name,
                move |_key, doc_bytes| {
                    if !fast_preds.is_empty() {
                        // Fast path: evaluate all conditions directly on MsgPack
                        // bytes — no full rmp_serde deserialization needed.
                        fast_preds.iter().all(|(field, op, val)| {
                            query::evaluate_predicate_msgpack(doc_bytes, field, op, val)
                                .unwrap_or(false)
                        })
                    } else {
                        // Full WHERE clause (e.g. $or/$and) — still on raw bytes.
                        query::evaluate_where_msgpack(doc_bytes, &clause).unwrap_or(false)
                    }
                },
                0,
                early_exit_count,
                params.default_order_asc,
                params.has_where,
            )
        }

        // Prefix only: BTreeMap insertion-order scan filtered by key prefix.
        ScanStrategy::PrefixOnly => {
            let pfxs: Vec<String> = allowed_prefixes_strings
                .unwrap_or_default()
                .into_iter()
                .map(str::to_string)
                .collect();

            db.get_filtered(
                params.col_name,
                move |key, _| pfxs.iter().any(|p| key.starts_with(p.as_str())),
                0,
                early_exit_count,
                params.default_order_asc,
                params.has_where,
            )
        }

        // WHERE + prefix: single pass combining both checks.
        ScanStrategy::WhereAndPrefix => {
            let clause = params.where_clause.unwrap().clone();
            let fast_preds = extract_fast_predicates(&clause);
            let pfxs: Vec<String> = allowed_prefixes_strings
                .unwrap_or_default()
                .into_iter()
                .map(str::to_string)
                .collect();

            db.get_filtered(
                params.col_name,
                move |key, doc_bytes| {
                    if !pfxs.iter().any(|p| key.starts_with(p.as_str())) {
                        return false;
                    }
                    if !fast_preds.is_empty() {
                        fast_preds.iter().all(|(field, op, val)| {
                            query::evaluate_predicate_msgpack(doc_bytes, field, op, val)
                                .unwrap_or(false)
                        })
                    } else {
                        query::evaluate_where_msgpack(doc_bytes, &clause).unwrap_or(false)
                    }
                },
                0,
                early_exit_count,
                params.default_order_asc,
                params.has_where,
            )
        }

        // Full scan: trivially-true predicate; BTreeMap seq_index gives
        // insertion-order results with early-exit.
        ScanStrategy::FullScan => db.get_filtered(
            params.col_name,
            |_, _| true,
            0,
            early_exit_count,
            params.default_order_asc,
            false,
        ),
    }
}

/// Extract all AND-conditions from a WHERE clause as a flat list of
/// `(field_path, operator, value)` triples that can each be evaluated
/// directly on raw MsgPack bytes via `query::evaluate_predicate_msgpack`.
///
/// Returns a **non-empty** Vec when every condition in the clause is
/// fast-path compatible. Returns an **empty** Vec (fall back to full
/// deserialization) when:
///   - Any top-level key starts with `$` (logical operators: `$or`, `$and`, �)
///   - Any operator is not in the fast-path set (e.g. unknown/custom operators)
///   - The clause is not a plain JSON object
///
/// Examples:
///   `{ "brand": "Apple" }`
///       � `[("brand", "$eq", "Apple")]`
///
///   `{ "price": { "$gte": 1500, "$lte": 2500 } }`
///       � `[("price", "$gte", 1500), ("price", "$lte", 2500)]`
///
///   `{ "in_stock": true, "price": { "$lt": 1000 } }`
///       � `[("in_stock", "$eq", true), ("price", "$lt", 1000)]`
///
///   - Any operator is not in the fast-path set (e.g. unknown/custom operators)
///       � `[]`  (requires full deserialization)
fn extract_fast_predicates(clause: &Value) -> Vec<(String, String, Value)> {
    // Handle array format: [{...}, {...}] — implicit AND; flatten all elements.
    if let Some(arr) = clause.as_array() {
        let mut result = Vec::new();
        for item in arr {
            let preds = extract_fast_predicates(item);
            if preds.is_empty() {
                return Vec::new();
            }
            result.extend(preds);
        }
        return result;
    }

    let obj = match clause.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };

    let mut result = Vec::new();

    for (field, condition) in obj {
        // Logical operators ($or, $and, $not, �) require full deserialization.
        if field.starts_with('$') {
            return Vec::new();
        }

        match condition {
            // Implicit equality: { "field": scalar }
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                result.push((
                    field.clone(),
                    WhereOperator::Eq.as_str().to_string(),
                    condition.clone(),
                ));
            }
            // Explicit operator(s): { "field": { "$op": value, � } }
            Value::Object(op_obj) => {
                for (op, op_val) in op_obj {
                    if !is_fast_path_op(op) {
                        // Unsupported operator � bail out entirely.
                        return Vec::new();
                    }
                    result.push((field.clone(), op.clone(), op_val.clone()));
                }
            }
            // Null, Array, or nested object � not directly comparable; fall back.
            _ => return Vec::new(),
        }
    }

    result
}
