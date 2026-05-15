use serde_json::Value;
use crate::{engine, query};

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
