pub(crate) const ALLOWED_PROPERTIES: &[&str] = &[
    "collection", "keys", "where", "fields", "excludedFields",
    "joins", "sort", "count", "offset", "_allowed_prefixes"
];

// src/core/constants.rs

pub struct SystemFields;

impl SystemFields {
    // Public API field names (returned to clients).
    pub const VERSION: &'static str     = "_v";
    pub const KEY: &'static str         = "_key";
    // todo in v2 long property names will be removed completely
    pub const SEQ: &'static str         = "_seq";
    pub const CREATED_AT: &'static str  = "_createdAt";
    pub const MODIFIED_AT: &'static str = "_modifiedAt";
    pub const EXPIRES_AT: &'static str  = "_expiresAt";

    // Compact internal storage field names (stored in MsgPack / WAL).
    // Saves ~17 bytes per document. Expanded back to full names at read time.
    pub const STORE_SEQ: &'static str         = "_s";
    pub const STORE_CREATED_AT: &'static str  = "_ca";
    pub const STORE_MODIFIED_AT: &'static str = "_ma";
}