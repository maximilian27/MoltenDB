use crate::common::payload_fields::PayloadField;

pub(crate) const SCHEMA_ALLOWED: &[&str] = &[
    PayloadField::Collection.as_str(),
    PayloadField::Schema.as_str(),
    PayloadField::Ttl.as_str(),
    PayloadField::MaxSize.as_str(),
];
