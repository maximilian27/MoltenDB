use crate::common::payload_fields::PayloadField;

pub(crate) const UPDATE_ALLOWED: &[&str] = &[
    PayloadField::Collection.as_str(),
    PayloadField::Data.as_str(),
];
