// src/handlers/delete/constants.rs

pub(crate) const DELETE_ALLOWED: &[&str] = &["collection", "keys", "where", "count", "drop"];
pub(crate) const DEFAULT_DELETE_COUNT: usize = 100;
pub(crate) const MAX_DELETE_COUNT: usize = 1_000;