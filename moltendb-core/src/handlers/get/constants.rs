use crate::common::payload_fields::PayloadField;

pub(crate) const GET_ALLOWED: &[&str] = &[
    PayloadField::Collection.as_str(),
    PayloadField::Where.as_str(),
    PayloadField::Fields.as_str(),
    PayloadField::ExcludedFields.as_str(),
    PayloadField::Joins.as_str(),
    PayloadField::Sort.as_str(),
    PayloadField::Count.as_str(),
    PayloadField::Offset.as_str(),
    PayloadField::AllowedPrefixes.as_str(),
];

// src/core/constants.rs
