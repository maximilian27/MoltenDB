// -- Parsed query parameters --------------------------------------------------

use serde_json::Value;

pub struct GetParams<'a> {
    pub(crate) col_name: &'a str,
    pub(crate) where_clause: Option<&'a Value>,
    pub(crate) joins_req: Option<&'a Vec<Value>>,
    pub(crate) sort_specs: Option<Vec<Value>>,
    pub(crate) count_limit: usize,
    pub(crate) offset: usize,
    pub(crate) fields_req: Option<&'a Vec<Value>>,
    pub(crate) excluded_req: Option<&'a Vec<Value>>,
    pub(crate) allowed_prefixes: Option<&'a Vec<Value>>,
    pub(crate) expires_val: Option<Value>,
}

pub struct FetchParams<'a> {
    pub(crate) col_name: &'a str,
    pub(crate) payload: &'a Value,
    pub(crate) where_clause: Option<&'a Value>,
    pub(crate) has_joins: bool,
    pub(crate) has_sort: bool,
    pub(crate) has_where: bool,
    pub(crate) default_order_asc: bool,
    pub(crate) offset: usize,
    pub(crate) count_limit: usize,
    pub(crate) allowed_prefixes: Option<&'a Vec<Value>>,
}
