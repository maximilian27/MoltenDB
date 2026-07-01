use crate::handlers::common::errors::OperationError;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum GetError {
    CountExceeded(usize),
    CollectionExpired,
    WhereEvalError(String),
    NoDocumentsFound,
    OrderSortMutuallyExclusive,
    InvalidOrderValue,
}

impl fmt::Display for GetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GetError::CountExceeded(max) => write!(f, "'count' cannot exceed {max}"),
            GetError::CollectionExpired => write!(f, "No documents found"),
            GetError::WhereEvalError(details) => write!(f, "{details}"),
            GetError::NoDocumentsFound => write!(f, "No documents found"),
            GetError::OrderSortMutuallyExclusive => {
                write!(f, "'order' and 'sort' cannot be used together")
            }
            GetError::InvalidOrderValue => write!(f, "'order' must be \"asc\" or \"desc\""),
        }
    }
}

impl Error for GetError {}

impl OperationError for GetError {
    fn status_code(&self) -> u16 {
        match self {
            GetError::CountExceeded(_) => 400,
            GetError::WhereEvalError(_) => 400,
            GetError::OrderSortMutuallyExclusive => 400,
            GetError::InvalidOrderValue => 400,
            GetError::CollectionExpired | GetError::NoDocumentsFound => 404,
        }
    }
}
