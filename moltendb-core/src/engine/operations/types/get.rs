/// Parameters for the [`get_filtered`] operation.
///
/// Grouping these into a struct keeps the function signature within Clippy's
/// argument-count limit and makes call sites more readable.
pub struct GetFilteredParams<'a, P>
where
    P: Fn(&str, &[u8]) -> bool + Sync + Send,
{
    pub collection: &'a str,
    pub predicate: P,
    pub offset: usize,
    pub count: Option<usize>,
    pub default_order_asc: bool,
    pub has_where: bool,
}

/// Parameters for the [`scan_top_n`] operation.
///
/// Grouping these into a struct keeps the function signature within Clippy's
/// argument-count limit and makes call sites more readable.
pub struct ScanTopNParams<'a, P, C>
where
    P: Fn(&str, &[u8]) -> bool + Sync,
    C: Fn(&serde_json::Value, &serde_json::Value) -> std::cmp::Ordering + Send + Sync,
{
    pub collection: &'a str,
    pub predicate: P,
    pub cmp: C,
    pub cap: usize,
}

/// Parameters for the [`scan_top_n_raw`] operation.
///
/// Grouping these into a struct keeps the function signature within Clippy's
/// argument-count limit and makes call sites more readable.
pub struct ScanTopNRawParams<'a, P>
where
    P: Fn(&str, &[u8]) -> bool + Sync + Send,
{
    pub collection: &'a str,
    pub predicate: P,
    pub sort_field: &'a str,
    pub is_descending: bool,
    pub cap: usize,
}
