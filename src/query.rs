// ─── query.rs ─────────────────────────────────────────────────────────────────
// This file implements the query engine — the logic that filters, projects,
// and evaluates conditions on JSON documents.
//
// Three main capabilities:
//
//   1. Field projection (project / exclude)
//      Select only specific fields from a document, or exclude specific fields.
//      Both support dot-notation for nested fields (e.g. "meta.logins").
//
//   2. Nested value access (get_nested_value / insert_nested_value)
//      Helper functions to read or write a value at any depth in a JSON object
//      using a dot-separated path like "meta.shopping_cart.active_order".
//
//   3. WHERE clause evaluation (evaluate_where)
//      Evaluate a MongoDB-style query object against a document.
//      Supports: $eq, $ne, $gt, $gte, $lt, $lte, $contains, $or, $and, and implicit equality.
// ─────────────────────────────────────────────────────────────────────────────

// Value = dynamically-typed JSON value.
// Map = serde_json's ordered map type (used for JSON objects).
use serde_json::{Value, Map};

/// Select only the specified fields from a document (field projection).
///
/// This is similar to SQL's SELECT col1, col2 or GraphQL's field selection.
/// Fields are specified as dot-notation strings (e.g. "meta.logins").
/// Nested fields are reconstructed in the output — selecting "meta.logins"
/// from { name: "Alice", meta: { logins: 10, role: "admin" } } produces
/// { meta: { logins: 10 } } — only the requested nested field, not the whole
/// parent object.
///
/// Fields that don't exist in the document are silently skipped.
pub fn project(doc: &Value, fields: &[Value]) -> Value {
    // Start with an empty output object.
    let mut filtered_doc = Map::new();

    for field in fields {
        // Each field must be a string (e.g. "name" or "meta.logins").
        if let Some(field_path) = field.as_str() {
            // Split the dot-notation path into parts: "meta.logins" → ["meta", "logins"]
            let parts: Vec<&str> = field_path.split('.').collect();

            // Try to read the value at this path from the source document.
            if let Some(val) = get_nested_value(doc, &parts) {
                // Write the value into the output document at the same path,
                // creating intermediate objects as needed.
                insert_nested_value(&mut filtered_doc, &parts, val);
            }
        }
    }
    Value::Object(filtered_doc)
}

/// Read a value at any depth from a JSON object using a dot-notation path.
///
/// `parts` is the path split by '.': ["meta", "logins"] reads doc.meta.logins.
///
/// Returns `Some(value)` if the full path exists, `None` if any part is missing.
/// The returned value is cloned — the caller owns it.
pub fn get_nested_value(doc: &Value, parts: &[&str]) -> Option<Value> {
    let mut current = doc;
    for part in parts {
        // Try to descend one level. If the key doesn't exist, return None.
        if let Some(v) = current.get(*part) {
            current = v;
        } else {
            return None; // Path doesn't exist in this document
        }
    }
    // Clone the final value so the caller owns it independently of the document.
    Some(current.clone())
}

/// Write a value at any depth into a JSON object, creating intermediate
/// objects as needed.
///
/// Example: insert_nested_value(map, ["meta", "logins"], 10)
///   → map becomes { "meta": { "logins": 10 } }
///   If "meta" already exists as an object, "logins" is added to it.
///   If "meta" doesn't exist, it is created as a new empty object first.
fn insert_nested_value(target: &mut Map<String, Value>, parts: &[&str], value: Value) {
    // Base case: empty path — nothing to insert.
    if parts.is_empty() { return; }

    let key = parts[0].to_string();

    if parts.len() == 1 {
        // Base case: we're at the final key — insert the value directly.
        target.insert(key, value);
    } else {
        // Recursive case: we need to go deeper.
        // Get or create the intermediate object at `key`.
        // `entry().or_insert_with()` inserts a new empty object if the key
        // doesn't exist, then returns a mutable reference to the value.
        let next_target = target.entry(key).or_insert_with(|| Value::Object(Map::new()));

        // Recurse into the next level of the object.
        if let Some(next_map) = next_target.as_object_mut() {
            insert_nested_value(next_map, &parts[1..], value);
        }
    }
}

/// Remove specified fields from a document (the inverse of project).
///
/// Returns a copy of the document with the listed fields removed.
/// Supports dot-notation for nested fields (e.g. "meta.logins" removes only
/// the `logins` field inside `meta`, leaving other fields in `meta` intact).
///
/// If removing a nested field leaves its parent object empty, the parent
/// object is also removed (e.g. removing "meta.logins" from { meta: { logins: 10 } }
/// removes the entire `meta` key since it would be empty).
///
/// Note: `fields` and `excludedFields` are mutually exclusive — the handler
/// validates this before calling either function.
pub fn exclude(doc: &Value, fields: &[Value]) -> Value {
    // Clone the document into a mutable Map so we can remove fields from it.
    let mut result = match doc.as_object() {
        Some(obj) => obj.clone(),
        // If the document isn't an object (e.g. it's a string or number),
        // return it unchanged — nothing to exclude.
        None => return doc.clone(),
    };

    for field in fields {
        if let Some(field_path) = field.as_str() {
            // Split the dot-notation path into parts.
            let parts: Vec<&str> = field_path.split('.').collect();
            remove_nested_value(&mut result, &parts);
        }
    }
    Value::Object(result)
}

/// Remove a value at any depth from a JSON object, leaving sibling keys intact.
///
/// After removing a nested field, if the parent object is now empty, the
/// parent is also removed. This prevents leaving empty `{}` objects behind.
fn remove_nested_value(target: &mut Map<String, Value>, parts: &[&str]) {
    // Base case: empty path — nothing to remove.
    if parts.is_empty() { return; }

    let key = parts[0];

    if parts.len() == 1 {
        // Base case: remove the key directly from this object.
        target.remove(key);
    } else if let Some(child) = target.get_mut(key) {
        // Recursive case: descend into the child object and remove from there.
        if let Some(child_map) = child.as_object_mut() {
            remove_nested_value(child_map, &parts[1..]);
        }
        // After recursing, check if the child object is now empty.
        // If so, remove the parent key too — no point keeping an empty {}.
        if target.get(key).and_then(|v| v.as_object()).map(|o| o.is_empty()).unwrap_or(false) {
            target.remove(key);
        }
    }
}

/// Evaluate a MongoDB-style WHERE clause against a single document.
///
/// Returns `true` if the document matches all conditions, `false` otherwise.
///
/// The query object is a JSON object where each key is either:
///   - A field name (possibly dot-notation): { "role": "admin" }
///   - A logical operator: { "$or": [...], "$and": [...] }
///
/// Field conditions can be:
///   - An implicit equality: { "role": "admin" } (shorthand for $eq)
///   - An operator object: { "age": { "$gt": 18 } }
///
/// Supported operators:
///   Equality:   $eq / $equals,  $ne / $notEquals
///   Numeric:    $gt / $greaterThan,  $gte,  $lt / $lessThan,  $lte
///   String:     $contains / $ct
///   Logical:    $or (array of sub-queries, any must match)
///               $and (array of sub-queries, all must match)
pub fn evaluate_where(doc: &Value, query: &Value) -> bool {
    // If the query is not an object (e.g. null or a string), it passes automatically.
    let query_obj = match query.as_object() {
        Some(obj) => obj,
        None => return true, // No conditions = everything matches
    };

    // Every condition in the query object must pass (implicit AND at the top level).
    for (key, condition) in query_obj {

        // ── Logical operators ($or / $and) ────────────────────────────────────
        if key == "$or" {
            let sub_queries = match condition.as_array() {
                Some(arr) => arr,
                None => return false,
            };
            if !sub_queries.iter().any(|sub| evaluate_where(doc, sub)) { return false; }
            continue;
        }

        if key == "$and" {
            let sub_queries = match condition.as_array() {
                Some(arr) => arr,
                None => return false,
            };
            if !sub_queries.iter().all(|sub| evaluate_where(doc, sub)) { return false; }
            continue;
        }

        // ── Field condition ───────────────────────────────────────────────────
        // The key is a field name (possibly dot-notation like "meta.logins").
        // Split it into path parts and read the value from the document.
        let parts: Vec<&str> = key.split('.').collect();
        let doc_val_opt = get_nested_value(doc, &parts);

        // ── Implicit equality ─────────────────────────────────────────────────
        // { "role": "admin" } is shorthand for { "role": { "$eq": "admin" } }
        if !condition.is_object() {
            if let Some(dv) = &doc_val_opt {
                // The document's field value must equal the condition value.
                if dv != condition { return false; }
            } else {
                // The field doesn't exist in the document — condition fails.
                return false;
            }
            continue;
        }

        // ── Operator matching ─────────────────────────────────────────────────
        // { "age": { "$gt": 18, "$lt": 65 } }
        // All operators in the condition object must pass.
        let cond_obj = condition.as_object().unwrap();
        // Use Null as the document value if the field doesn't exist.
        let doc_val_ref = doc_val_opt.as_ref().unwrap_or(&Value::Null);

        for (op, op_val) in cond_obj {
            let passed = match op.as_str() {
                // ── Equality operators ────────────────────────────────────────
                // Both shorthand ($eq) and verbose ($equals) are supported.
                "$eq" | "$equals" => doc_val_ref == op_val,
                "$ne" | "$notEquals" => doc_val_ref != op_val,


                // ── Numeric comparison operators ──────────────────────────────
                // Both shorthand ($gt) and verbose ($greaterThan) are supported.
                // as_f64() converts any JSON number to a 64-bit float for comparison.
                "$gt" | "$greaterThan" | "$gte" | "$lt" | "$lessThan" | "$lte" => {
                    if let (Some(d_num), Some(o_num)) = (doc_val_ref.as_f64(), op_val.as_f64()) {
                        match op.as_str() {
                            "$gt" | "$greaterThan" => d_num > o_num,
                            "$gte"                 => d_num >= o_num,
                            "$lt" | "$lessThan"    => d_num < o_num,
                            "$lte"                 => d_num <= o_num,
                            _ => false,
                        }
                    } else {
                        // One of the values is not a number — condition fails.
                        false
                    }
                },


                // ── Contains operator ─────────────────────────────────────────
                // Works on both strings (substring check) AND arrays (membership check).
                //
                // String example:
                //   { "role": { "$contains": "admin" } }
                //   → true if role is "superadmin", "admin", etc.
                //
                // Array example:
                //   { "colors": { "$contains": "red" } }
                //   → true if the colors array contains the value "red"
                //   → O(n) scan over the array elements (use object-map pattern for O(1))
                //
                // Both shorthand ($ct) and verbose ($contains) are supported.
                "$contains" | "$ct" => {
                    match doc_val_ref {
                        // String case: check if the document string contains the needle string.
                        Value::String(d_str) => {
                            if let Some(o_str) = op_val.as_str() {
                                d_str.contains(o_str)
                            } else {
                                false
                            }
                        }
                        // Array case: check if any element in the array equals the target value.
                        // `arr.contains(op_val)` does a deep equality check on each element.
                        // Works for any JSON value type: strings, numbers, booleans, objects.
                        Value::Array(arr) => arr.contains(op_val),
                        // Any other type (number, object, null) — condition fails.
                        _ => false,
                    }
                },



                // ── In operator ───────────────────────────────────────────────────
                // Checks if the document field value is one of the values in a list.
                //
                // Example:
                //   { "role": { "$in": ["admin", "superadmin"] } }
                //   → true if role is "admin" OR "superadmin"
                //
                // Works for any JSON value type: strings, numbers, booleans.
                // The operator value must be an array — if it's not, the condition fails.
                //
                // Both shorthand ($in) and verbose ($oneOf) are supported.
                "$in" | "$oneOf" => {
                    if let Some(allowed) = op_val.as_array() {
                        // Check if the document value matches any element in the list.
                        // `contains` does a deep equality check — works for any JSON type.
                        allowed.contains(doc_val_ref)
                    } else {
                        // The operator value is not an array — condition fails.
                        false
                    }
                },

                // ── Not-in operator ───────────────────────────────────────────────────
                // The inverse of $in — true if the field value is NOT in the list.
                //
                // Example:
                //   { "role": { "$nin": ["banned", "suspended"] } }
                //   → true if role is anything except "banned" or "suspended"
                //
                // Both shorthand ($nin) and verbose ($notIn) are supported.
                "$nin" | "$notIn" => {
                    if let Some(excluded) = op_val.as_array() {
                        // True only if the document value is NOT in the exclusion list.
                        !excluded.contains(doc_val_ref)
                    } else {
                        // The operator value is not an array — condition fails.
                        false
                    }
                },

                // Unknown operator — fail the condition rather than silently passing.
                _ => false,
            };

            // If any operator in the condition object fails, the whole condition fails.
            if !passed { return false; }
        }
    }

    // All conditions passed.
    true
}
