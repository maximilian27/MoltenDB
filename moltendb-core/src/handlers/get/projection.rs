use serde_json::Value;
use crate::query;

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
    let v_val        = doc.get("_v").cloned();
    let seq_val      = doc.get("_seq").cloned();
    let created_val  = doc.get("_createdAt").cloned();
    let modified_val = doc.get("_modifiedAt").cloned();

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
            if requested.contains(&"_seq")       { if let Some(v) = seq_val      { dst.insert("_seq".to_string(), v); } }
            if requested.contains(&"_createdAt") { if let Some(v) = created_val  { dst.insert("_createdAt".to_string(), v); } }
            if requested.contains(&"_modifiedAt"){ if let Some(v) = modified_val { dst.insert("_modifiedAt".to_string(), v); } }
            if requested.contains(&"_expiresAt") { if let Some(v) = expires_val  { dst.insert("_expiresAt".to_string(), v); } }
        }
        projected
    } else if let Some(ex) = excluded {
        let mut shaped = query::exclude(&doc, ex);
        // Strip optional metadata fields -- they are opt-in via `fields` only.
        if let Some(obj) = shaped.as_object_mut() {
            obj.remove("_seq");
            obj.remove("_createdAt");
            obj.remove("_modifiedAt");
            obj.remove("_expiresAt");
        }
        shaped
    } else {
        // No projection -- strip optional metadata fields (opt-in via `fields` only).
        let mut d = doc;
        if let Some(obj) = d.as_object_mut() {
            obj.remove("_seq");
            obj.remove("_createdAt");
            obj.remove("_modifiedAt");
            obj.remove("_expiresAt");
        }
        d
    };

    // _v and _key are always injected last -- they cannot be suppressed.
    if let Some(obj) = out.as_object_mut() {
        if let Some(v) = v_val { obj.insert("_v".to_string(), v); }
        obj.insert("_key".to_string(), Value::String(key.to_string()));
    }
    Some(out)
}
