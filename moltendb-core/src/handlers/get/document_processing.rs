use serde_json::Value;
use std::cmp::Ordering;
use crate::{engine, query};
use crate::engine::ttl::ms_to_iso;
use crate::handlers::get::constants::SystemFields;
// -- Sort----------------------------------------------------------

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

// -- Joins----------------------------------------------------------

/// Resolve cross-collection joins for a single document.
/// Embeds related documents under their alias field and returns the list of alias names
/// (used later by `shape_doc` to exclude join aliases from projection).
pub fn apply_joins(db: &engine::Db, doc: &mut Value, joins: &[Value]) -> Vec<String> {
    let mut join_aliases: Vec<String> = Vec::new();

    for join_spec in joins {
        let Some(obj) = join_spec.as_object() else { continue };
        let Some((alias, inner)) = obj.iter().find_map(|(k, v)| {
            v.as_object().filter(|o| o.contains_key("from")).map(|o| (k.clone(), o))
        }) else { continue };

        let from     = inner.get("from").and_then(|f| f.as_str()).unwrap_or("");
        let on       = inner.get("on").and_then(|f| f.as_str()).unwrap_or("");
        let j_fields = inner.get("fields").and_then(|f| f.as_array());

        // Resolve the foreign key value (supports dot-notation).
        let fk_val = on.split('.').fold(Some(doc as &Value), |cur, part| {
            cur.and_then(|v| v.get(part))
        }).and_then(|v| v.as_str()).map(|s| s.to_string());

        if let Some(fk) = fk_val {
            if let Some(related) = db.get(from, vec![fk.clone()]).remove(&fk) {
                let embedded = if let Some(jf) = j_fields {
                    query::project(&related, jf)
                } else {
                    related
                };
                if let Some(doc_obj) = doc.as_object_mut() {
                    doc_obj.insert(alias.clone(), embedded);
                }
                join_aliases.push(alias);
            }
        }
    }

    join_aliases
}

// -- Timestamp conversion -------------------------------------------------------

/// Convert a timestamp value to an ISO 8601 string `Value`.
/// If the value is a `u64` Unix millisecond timestamp, convert it.
/// If it is already a string (legacy ISO format), return it unchanged.
fn ts_to_iso_val(v: Value) -> Value {
    if let Some(ms) = v.as_u64() {
        Value::String(ms_to_iso(ms))
    } else {
        v
    }
}

// -- Shape doc response----------------------------------------------------------

/// Apply field projection or exclusion to a document, preserving `_v`, `_seq`, `_createdAt`,
/// `_modifiedAt`, `_expiresAt` and inserting `_key`.
///
/// Returns `Some(shaped_doc)`. The caller may remove `_key` for single-key responses.
pub fn shape_doc(
    doc: Value,
    key: &str,
    fields: Option<&Vec<Value>>,
    excluded: Option<&Vec<Value>>,
    join_aliases: &[String],
    expires_val: Option<Value>,
) -> Option<Value> {
    // _v and _key are protocol primitives -- always present, cannot be suppressed.
    // _seq, _createdAt, _modifiedAt, _expiresAt are opt-in: only returned when
    // explicitly listed in a `fields` projection. They are stripped in all other cases.
    let v_val        = doc.get(SystemFields::VERSION).cloned();
    // Read compact storage names first, fall back to legacy long names for old docs.
    let seq_val      = doc.get(SystemFields::STORE_SEQ).or_else(|| doc.get(SystemFields::SEQ)).cloned();
    // Convert u64 Unix ms timestamps to ISO 8601 strings for the API response.
    // Legacy docs may already have ISO strings -- leave those unchanged.
    let created_val  = doc.get(SystemFields::STORE_CREATED_AT).or_else(|| doc.get(SystemFields::CREATED_AT)).cloned().map(ts_to_iso_val);
    let modified_val = doc.get(SystemFields::STORE_MODIFIED_AT).or_else(|| doc.get(SystemFields::MODIFIED_AT)).cloned().map(ts_to_iso_val);

    let mut out = if let Some(f) = fields {
        let mut projected = query::project(&doc, f);
        // Re-attach any joined sub-documents that were embedded before projection.
        if let (Some(src), Some(dst)) = (doc.as_object(), projected.as_object_mut()) {
            for alias in join_aliases {
                if let Some(v) = src.get(alias) {
                    dst.insert(alias.clone(), v.clone());
                }
            }
        }
        // Fields projection: re-attach optional metadata only if explicitly listed in `fields`.
        if let Some(dst) = projected.as_object_mut() {
            let requested: Vec<&str> = f.iter().filter_map(|v| v.as_str()).collect();
            // Always insert under the public API name regardless of how it was stored.
            if requested.contains(&SystemFields::SEQ)        { if let Some(v) = seq_val      { dst.insert(SystemFields::SEQ.to_string(), v); } }
            if requested.contains(&SystemFields::CREATED_AT) { if let Some(v) = created_val  { dst.insert(SystemFields::CREATED_AT.to_string(), v); } }
            if requested.contains(&SystemFields::MODIFIED_AT){ if let Some(v) = modified_val { dst.insert(SystemFields::MODIFIED_AT.to_string(), v); } }
            if requested.contains(&SystemFields::EXPIRES_AT) { if let Some(v) = expires_val  { dst.insert(SystemFields::EXPIRES_AT.to_string(), v); } }
        }
        projected
    } else if let Some(ex) = excluded {
        let mut shaped = query::exclude(&doc, ex);
        // Strip optional metadata fields -- they are opt-in via `fields` only.
        if let Some(obj) = shaped.as_object_mut() {
            obj.remove(SystemFields::SEQ);          obj.remove(SystemFields::STORE_SEQ);
            obj.remove(SystemFields::CREATED_AT);   obj.remove(SystemFields::STORE_CREATED_AT);
            obj.remove(SystemFields::MODIFIED_AT);  obj.remove(SystemFields::STORE_MODIFIED_AT);
            obj.remove(SystemFields::EXPIRES_AT);  obj.remove(SystemFields::STORE_EXPIRES_AT);
        }
        shaped
    } else {
        // No projection -- strip optional metadata fields (opt-in via `fields` only).
        let mut d = doc;
        if let Some(obj) = d.as_object_mut() {
            obj.remove(SystemFields::SEQ);          obj.remove(SystemFields::STORE_SEQ);
            obj.remove(SystemFields::CREATED_AT);   obj.remove(SystemFields::STORE_CREATED_AT);
            obj.remove(SystemFields::MODIFIED_AT);  obj.remove(SystemFields::STORE_MODIFIED_AT);
            obj.remove(SystemFields::EXPIRES_AT);  obj.remove(SystemFields::STORE_EXPIRES_AT);
        }
        d
    };

    // _v and _key are always injected last -- they cannot be suppressed.
    if let Some(obj) = out.as_object_mut() {
        if let Some(v) = v_val { obj.insert(SystemFields::VERSION.to_string(), v); }
        obj.insert(SystemFields::KEY.to_string(), Value::String(key.to_string()));
    }
    Some(out)
}

