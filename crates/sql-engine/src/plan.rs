//! Typed SQL plans accepted by the deterministic `MongoDB` executor.

use mongo_pg_schema_discovery::FieldPath;

/// A parsed statement approved for deterministic execution.
#[derive(Debug, Clone, PartialEq)]
pub enum StatementPlan {
    Select(SelectPlan),
    Insert(InsertPlan),
    Update(UpdatePlan),
    Delete(DeletePlan),
}

/// A supported read query plan.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectPlan {
    pub collection: String,
    pub projection: Projection,
    pub filter: Option<Predicate>,
    pub limit: Option<u64>,
}

/// A projection permitted by the first read path.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    All,
    Fields(Vec<ProjectedField>),
}

/// A selected field and its optional SQL alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedField {
    pub path: FieldPath,
    pub alias: Option<String>,
}

/// A supported insert plan.
#[derive(Debug, Clone, PartialEq)]
pub struct InsertPlan {
    pub collection: String,
    pub columns: Vec<FieldPath>,
    pub rows: Vec<Vec<SqlValue>>,
}

/// A supported update plan.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdatePlan {
    pub collection: String,
    pub assignments: Vec<AssignmentPlan>,
    pub filter: Predicate,
}

/// One deterministic update assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignmentPlan {
    pub path: FieldPath,
    pub value: SqlValue,
}

/// A supported delete plan.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletePlan {
    pub collection: String,
    pub filter: Predicate,
}

/// Supported scalar values and prepared-statement placeholders.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Boolean(bool),
    Integer(i64),
    FloatingPoint(f64),
    String(String),
    Placeholder(String),
}

/// A predicate that can be translated by the deterministic executor.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Compare {
        path: FieldPath,
        operator: ComparisonOperator,
        value: SqlValue,
    },
    In {
        path: FieldPath,
        values: Vec<SqlValue>,
        negated: bool,
    },
    IsNull {
        path: FieldPath,
        negated: bool,
    },
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
}

/// Comparison operators allowed in the MVP predicate subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOperator {
    Equal,
    NotEqual,
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
}
