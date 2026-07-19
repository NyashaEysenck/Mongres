//! Strict `PostgreSQL` SQL parsing and lowering into typed execution plans.
//!
//! This crate accepts a deliberately small SQL subset. It does not create
//! `MongoDB` operations; that responsibility belongs to the executor crate.

mod lowering;
mod plan;
mod resolve;
mod type_policy;

pub use lowering::parse_sql;
pub use plan::{
    AssignmentPlan, ComparisonOperator, DeletePlan, InsertPlan, Predicate, ProjectedField,
    Projection, SelectPlan, SqlValue, StatementPlan, UpdatePlan,
};

/// Constructs the standard error used for SQL features outside the MVP.
#[must_use]
pub fn unsupported_feature(feature: &str) -> mongo_pg_common::ProxyError {
    mongo_pg_common::ProxyError::new(
        mongo_pg_common::ErrorKind::FeatureNotSupported,
        format!("SQL feature is not supported: {feature}"),
    )
}

#[cfg(test)]
mod tests;
