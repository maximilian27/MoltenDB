pub mod projection;
pub mod sort;
pub mod fetch;
pub mod joins;

pub use projection::shape_doc;
pub use sort::{compare_values, make_comparator};
pub use fetch::fetch_documents;
pub use joins::apply_joins;
