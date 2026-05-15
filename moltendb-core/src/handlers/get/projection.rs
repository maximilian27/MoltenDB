use serde_json::Value;
use crate::query;

/// Apply field projection or exclusion to a document, preserving `_v`, `_createdAt`,
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
    let v_val        = doc.get("_v").cloned();
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
        projected
    } else if let Some(ex) = excluded {
        query::exclude(&doc, ex)
    } else {
        doc
    };

    if let Some(obj) = out.as_object_mut() {
        if let Some(v) = v_val        { obj.insert("_v".to_string(), v); }
        if let Some(v) = created_val  { obj.insert("_createdAt".to_string(), v); }
        if let Some(v) = modified_val { obj.insert("_modifiedAt".to_string(), v); }
        // _expiresAt is a virtual field -- inject it if the collection has a TTL.
        if let Some(v) = expires_val  { obj.insert("_expiresAt".to_string(), v); }
        obj.insert("_key".to_string(), Value::String(key.to_string()));
    }
    Some(out)
}
