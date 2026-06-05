/// All the fields that can be passed with a payload.
#[allow(dead_code)]
pub enum PayloadField {
    AllowedPrefixes,
    Collection,
    Count,
    Data,
    Drop,
    ExcludedFields,
    Fields,
    From,
    Joins,
    Keys,
    MaxSize,
    Offset,
    On,
    Schema,
    Sort,
    Ttl,
    Where,
}

#[allow(dead_code)]
impl PayloadField {
    pub fn as_str(&self) -> &'static str {
        match self {
            PayloadField::AllowedPrefixes => "allowed_prefixes",
            PayloadField::Collection => "collection",
            PayloadField::Count => "count",
            PayloadField::Data => "data",
            PayloadField::Drop => "drop",
            PayloadField::ExcludedFields => "excluded_fields",
            PayloadField::Fields => "fields",
            PayloadField::From => "from",
            PayloadField::Joins => "joins",
            PayloadField::Keys => "keys",
            PayloadField::MaxSize => "max_size",
            PayloadField::Offset => "offset",
            PayloadField::On => "on",
            PayloadField::Schema => "schema",
            PayloadField::Sort => "sort",
            PayloadField::Ttl => "ttl",
            PayloadField::Where => "where",
        }
    }
}
