use serde_json::Value;
use std::cmp::Ordering;
use crate::query;

/// Compare two optional JSON values for sorting.
/// Numbers -> numeric, strings -> lexicographic, missing/null -> sorts last.
pub fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None)       => Ordering::Equal,
        (None, Some(_))    => Ordering::Greater,
        (Some(_), None)    => Ordering::Less,
        (Some(va), Some(vb)) => {
            if let (Some(na), Some(nb)) = (va.as_f64(), vb.as_f64()) {
                return na.partial_cmp(&nb).unwrap_or(Ordering::Equal);
            }
            if let (Some(sa), Some(sb)) = (va.as_str(), vb.as_str()) {
                return sa.cmp(sb);
            }
            va.to_string().cmp(&vb.to_string())
        }
    }
}

/// Build a sort comparator from a `sort` spec array.
/// Each spec is either a plain string field name or `{ "field": "...", "order": "asc"|"desc" }`.
pub fn make_comparator(specs: Vec<Value>) -> impl Fn(&Value, &Value) -> Ordering {
    move |a: &Value, b: &Value| {
        for spec in &specs {
            let (field, descending) = if let Some(s) = spec.as_str() {
                (s.to_string(), false)
            } else if let Some(obj) = spec.as_object() {
                let f = obj.get("field").and_then(|f| f.as_str()).unwrap_or("").to_string();
                let d = obj.get("order").and_then(|o| o.as_str())
                    .map(|o| o.eq_ignore_ascii_case("desc"))
                    .unwrap_or(false);
                (f, d)
            } else {
                continue;
            };
            if field.is_empty() { continue; }
            let parts: Vec<&str> = field.split('.').collect();
            let ord = compare_values(
                query::get_nested_value(a, &parts).as_ref(),
                query::get_nested_value(b, &parts).as_ref(),
            );
            if ord != Ordering::Equal {
                return if descending { ord.reverse() } else { ord };
            }
        }
        Ordering::Equal
    }
}
