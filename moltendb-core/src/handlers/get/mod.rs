pub mod document_processing;
pub mod fetch;
pub(crate) mod types;
pub(crate) mod constants;

pub use document_processing::{compare_values, make_comparator, apply_joins, shape_doc};
pub use fetch::fetch_documents;
