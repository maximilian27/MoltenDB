use crate::common::payload_fields::PayloadField;

pub(crate) const SET_ALLOWED: &[&str] = &[
    PayloadField::Collection.as_str(),
    PayloadField::Data.as_str(),
    PayloadField::Ttl.as_str(),
    PayloadField::MaxSize.as_str(),
];
