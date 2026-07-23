// src/handlers/delete/constants.rs

use crate::common::payload_fields::PayloadField;

pub(crate) const DELETE_ALLOWED: &[&str] = &[
    PayloadField::Collection.as_str(),
    PayloadField::Count.as_str(),
    PayloadField::Drop.as_str(),
    PayloadField::Keys.as_str(),
    PayloadField::Order.as_str(),
    PayloadField::Where.as_str(),
];
pub(crate) const DEFAULT_DELETE_COUNT: usize = 100;
pub(crate) const MAX_DELETE_COUNT: usize = 1_000;
