use serde_json::Value;
use crate::query;

const VERSION: &str = "_v";
const KEY: &str = "_key";
const SEQ: &str = "_seq";
const CREATED_AT: &str = "_createdAt";
const MODIFIED_AT: &str = "_modifiedAt";
const EXPIRES_AT: &str = "_expiresAt";
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
    let v_val        = doc.get(VERSION).cloned();
    let seq_val      = doc.get(SEQ).cloned();
    let created_val  = doc.get(CREATED_AT).cloned();
    let modified_val = doc.get(MODIFIED_AT).cloned();

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
            if requested.contains(&SEQ)       { if let Some(v) = seq_val      { dst.insert("_seq".to_string(), v); } }
            if requested.contains(&CREATED_AT) { if let Some(v) = created_val  { dst.insert(CREATED_AT.to_string(), v); } }
            if requested.contains(&MODIFIED_AT){ if let Some(v) = modified_val { dst.insert(MODIFIED_AT.to_string(), v); } }
            if requested.contains(&EXPIRES_AT) { if let Some(v) = expires_val  { dst.insert("_expiresAt".to_string(), v); } }
        }
        projected
    } else if let Some(ex) = excluded {
        let mut shaped = query::exclude(&doc, ex);
        // Strip optional metadata fields -- they are opt-in via `fields` only.
        if let Some(obj) = shaped.as_object_mut() {
            obj.remove(SEQ);
            obj.remove(CREATED_AT);
            obj.remove(MODIFIED_AT);
            obj.remove(EXPIRES_AT);
        }
        shaped
    } else {
        // No projection -- strip optional metadata fields (opt-in via `fields` only).
        let mut d = doc;
        if let Some(obj) = d.as_object_mut() {
            obj.remove(SEQ);
            obj.remove(CREATED_AT);
            obj.remove(MODIFIED_AT);
            obj.remove(EXPIRES_AT);
        }
        d
    };

    // _v and _key are always injected last -- they cannot be suppressed.
    if let Some(obj) = out.as_object_mut() {
        if let Some(v) = v_val { obj.insert(VERSION.to_string(), v); }
        obj.insert(KEY.to_string(), Value::String(key.to_string()));
    }
    Some(out)
}
