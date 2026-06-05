#[allow(dead_code)]
pub enum WhereOperator {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    NotIn,
    Contains,
    Or,
    And,
}

#[allow(dead_code)]
impl WhereOperator {
    pub fn as_str(&self) -> &'static str {
        match self {
            WhereOperator::Eq => "$eq",
            WhereOperator::NotEq => "$ne",
            WhereOperator::Gt => "$gt",
            WhereOperator::Gte => "$gte",
            WhereOperator::Lt => "$lt",
            WhereOperator::Lte => "$lte",
            WhereOperator::In => "$in",
            WhereOperator::NotIn => "$nin",
            WhereOperator::Contains => "$ct",
            WhereOperator::Or => "$or",
            WhereOperator::And => "$and",
        }
    }

    pub fn aliases(&self) -> &'static [&'static str] {
        match self {
            WhereOperator::Eq => &["$eq", "$equals"],
            WhereOperator::NotEq => &["$ne", "$notEquals"],
            WhereOperator::Gt => &["$gt", "$greaterThan"],
            WhereOperator::Gte => &["$gte", "$greaterThanOrEqual"],
            WhereOperator::Lt => &["$lt", "$lessThan"],
            WhereOperator::Lte => &["$lte", "$lessThanOrEqual"],
            WhereOperator::In => &["$in", "$oneOf"],
            WhereOperator::NotIn => &["$nin", "$notIn"],
            WhereOperator::Contains => &["$ct", "$contains"],
            WhereOperator::Or => &["$or"],
            WhereOperator::And => &["$and"],
        }
    }

    pub fn from_str(s: &str) -> Option<WhereOperator> {
        let all = [
            WhereOperator::Eq,
            WhereOperator::NotEq,
            WhereOperator::Gt,
            WhereOperator::Gte,
            WhereOperator::Lt,
            WhereOperator::Lte,
            WhereOperator::In,
            WhereOperator::NotIn,
            WhereOperator::Contains,
            WhereOperator::Or,
            WhereOperator::And,
        ];
        all.into_iter().find(|op| op.aliases().contains(&s))
    }
}
