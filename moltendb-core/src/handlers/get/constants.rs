pub(crate) const ALLOWED_PROPERTIES: &[&str] = &[
    "collection", "keys", "where", "fields", "excludedFields",
    "joins", "sort", "count", "offset", "_allowed_prefixes"
];

// src/core/constants.rs

pub struct SystemFields;

impl SystemFields {
    pub const VERSION: &'static str     = "_v";
    pub const KEY: &'static str         = "_key";
    pub const SEQ: &'static str         = "_seq";
    pub const CREATED_AT: &'static str  = "_createdAt";
    pub const MODIFIED_AT: &'static str = "_modifiedAt";
    pub const EXPIRES_AT: &'static str  = "_expiresAt";
}