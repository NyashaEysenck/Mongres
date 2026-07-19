//! Strict `PostgreSQL` SQL parsing and lowering into typed execution plans.
//!
//! This crate accepts a deliberately small SQL subset. It does not create
//! `MongoDB` operations; that responsibility belongs to the executor crate.

use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_schema_discovery::{FieldPath, SchemaProfile};
use sqlparser::{
    ast::{
        AssignmentTarget, BinaryOperator, Delete, Expr, FromTable, GroupByExpr, Ident, Insert,
        ObjectName, Select, SelectItem, SetExpr, Statement, TableFactor, TableWithJoins, Value,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};

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

/// Parses exactly one SQL statement and lowers it into a schema-aware plan.
///
/// # Errors
///
/// Returns a `PostgreSQL`-mapped error for invalid syntax, unsupported SQL, or
/// fields that do not exist in the supplied schema profile.
pub fn parse_sql(
    sql: &str,
    collection_name: &str,
    schema: &SchemaProfile,
) -> Result<StatementPlan, ProxyError> {
    let statements = Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .map_err(|error| ProxyError::new(ErrorKind::Syntax, error.to_string()))?;
    let [statement] = statements.as_slice() else {
        return Err(invalid_input("exactly one SQL statement is required"));
    };

    match statement {
        Statement::Query(query) => parse_select(query, collection_name, schema),
        Statement::Insert(insert) => parse_insert(insert, collection_name, schema),
        Statement::Update {
            table,
            assignments,
            from,
            selection,
            returning,
            or,
        } => parse_update(
            table,
            assignments,
            from.as_ref(),
            selection.as_ref(),
            returning.as_ref(),
            *or,
            collection_name,
            schema,
        ),
        Statement::Delete(delete) => parse_delete(delete, collection_name, schema),
        _ => Err(unsupported_feature("statement type")),
    }
}

fn parse_select(
    query: &sqlparser::ast::Query,
    collection_name: &str,
    schema: &SchemaProfile,
) -> Result<StatementPlan, ProxyError> {
    if query.with.is_some()
        || query.order_by.is_some()
        || query.offset.is_some()
        || query.fetch.is_some()
        || !query.limit_by.is_empty()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
        || query.settings.is_some()
        || query.format_clause.is_some()
    {
        return Err(unsupported_feature("query modifiers"));
    }

    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(unsupported_feature("subqueries, VALUES, or set operations"));
    };
    validate_select_shape(select)?;
    let collection = collection_from_select(select, collection_name)?;
    let projection = parse_projection(&select.projection, &collection, schema)?;
    let filter = select
        .selection
        .as_ref()
        .map(|expression| parse_predicate(expression, &collection, schema))
        .transpose()?;
    let limit = query.limit.as_ref().map(parse_limit).transpose()?;

    Ok(StatementPlan::Select(SelectPlan {
        collection,
        projection,
        filter,
        limit,
    }))
}

fn validate_select_shape(select: &Select) -> Result<(), ProxyError> {
    let has_group_by = match &select.group_by {
        GroupByExpr::All(_) => true,
        GroupByExpr::Expressions(expressions, modifiers) => {
            !expressions.is_empty() || !modifiers.is_empty()
        }
    };
    if select.distinct.is_some()
        || select.top.is_some()
        || select.into.is_some()
        || !select.lateral_views.is_empty()
        || select.prewhere.is_some()
        || has_group_by
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || select.having.is_some()
        || !select.named_window.is_empty()
        || select.qualify.is_some()
        || select.connect_by.is_some()
    {
        return Err(unsupported_feature("SELECT clause"));
    }
    Ok(())
}

fn parse_insert(
    insert: &Insert,
    collection_name: &str,
    schema: &SchemaProfile,
) -> Result<StatementPlan, ProxyError> {
    let collection = collection_from_object_name(&insert.table_name, collection_name)?;
    if insert.table_alias.is_some()
        || insert.ignore
        || insert.overwrite
        || insert.partitioned.is_some()
        || !insert.after_columns.is_empty()
        || insert.table
        || insert.on.is_some()
        || insert.returning.is_some()
        || insert.replace_into
        || insert.priority.is_some()
        || insert.insert_alias.is_some()
    {
        return Err(unsupported_feature("INSERT modifier"));
    }
    if insert.columns.is_empty() {
        return Err(invalid_input("INSERT requires an explicit column list"));
    }
    let columns = insert
        .columns
        .iter()
        .map(|column| resolve_insert_column(column, &collection, schema))
        .collect::<Result<Vec<_>, _>>()?;
    let source = insert
        .source
        .as_ref()
        .ok_or_else(|| invalid_input("INSERT requires a VALUES clause"))?;
    if source.with.is_some()
        || source.order_by.is_some()
        || source.limit.is_some()
        || source.offset.is_some()
        || source.fetch.is_some()
        || !source.limit_by.is_empty()
        || !source.locks.is_empty()
    {
        return Err(unsupported_feature("INSERT query modifier"));
    }
    let SetExpr::Values(values) = source.body.as_ref() else {
        return Err(unsupported_feature("INSERT ... SELECT"));
    };
    if values.rows.is_empty() {
        return Err(invalid_input("INSERT requires at least one VALUES row"));
    }
    let rows = values
        .rows
        .iter()
        .map(|row| {
            if row.len() != columns.len() {
                return Err(invalid_input(
                    "each INSERT VALUES row must match the number of columns",
                ));
            }
            row.iter().map(parse_value).collect()
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(StatementPlan::Insert(InsertPlan {
        collection,
        columns,
        rows,
    }))
}

#[allow(clippy::too_many_arguments)]
fn parse_update(
    table: &TableWithJoins,
    assignments: &[sqlparser::ast::Assignment],
    from: Option<&TableWithJoins>,
    selection: Option<&Expr>,
    returning: Option<&Vec<SelectItem>>,
    conflict: Option<sqlparser::ast::SqliteOnConflict>,
    collection_name: &str,
    schema: &SchemaProfile,
) -> Result<StatementPlan, ProxyError> {
    if from.is_some() || returning.is_some() || conflict.is_some() {
        return Err(unsupported_feature("UPDATE modifier"));
    }
    let collection = collection_from_table_with_joins(table, collection_name)?;
    if assignments.is_empty() {
        return Err(invalid_input("UPDATE requires at least one assignment"));
    }
    let assignments = assignments
        .iter()
        .map(|assignment| {
            let AssignmentTarget::ColumnName(target) = &assignment.target else {
                return Err(unsupported_feature("tuple UPDATE assignment"));
            };
            Ok(AssignmentPlan {
                path: resolve_field(&target.0, &collection, schema)?,
                value: parse_value(&assignment.value)?,
            })
        })
        .collect::<Result<Vec<_>, ProxyError>>()?;
    let filter = selection
        .ok_or_else(|| invalid_input("UPDATE requires a WHERE clause in the MVP"))
        .and_then(|expression| parse_predicate(expression, &collection, schema))?;

    Ok(StatementPlan::Update(UpdatePlan {
        collection,
        assignments,
        filter,
    }))
}

fn parse_delete(
    delete: &Delete,
    collection_name: &str,
    schema: &SchemaProfile,
) -> Result<StatementPlan, ProxyError> {
    if !delete.tables.is_empty()
        || delete.using.is_some()
        || delete.returning.is_some()
        || !delete.order_by.is_empty()
        || delete.limit.is_some()
    {
        return Err(unsupported_feature("DELETE modifier"));
    }
    let tables = match &delete.from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
    };
    let [table] = tables.as_slice() else {
        return Err(unsupported_feature("multi-table DELETE"));
    };
    let collection = collection_from_table_with_joins(table, collection_name)?;
    let filter = delete
        .selection
        .as_ref()
        .ok_or_else(|| invalid_input("DELETE requires a WHERE clause in the MVP"))
        .and_then(|expression| parse_predicate(expression, &collection, schema))?;

    Ok(StatementPlan::Delete(DeletePlan { collection, filter }))
}

fn collection_from_select(select: &Select, collection_name: &str) -> Result<String, ProxyError> {
    let [table] = select.from.as_slice() else {
        return Err(unsupported_feature(
            "queries without exactly one FROM table",
        ));
    };
    collection_from_table_with_joins(table, collection_name)
}

fn collection_from_table_with_joins(
    table: &TableWithJoins,
    collection_name: &str,
) -> Result<String, ProxyError> {
    if !table.joins.is_empty() {
        return Err(unsupported_feature("JOIN"));
    }
    let TableFactor::Table { name, args, .. } = &table.relation else {
        return Err(unsupported_feature("non-table FROM source"));
    };
    if args.is_some() {
        return Err(unsupported_feature("table-valued function"));
    }
    collection_from_object_name(name, collection_name)
}

fn collection_from_object_name(
    name: &ObjectName,
    expected_collection: &str,
) -> Result<String, ProxyError> {
    let [identifier] = name.0.as_slice() else {
        return Err(unsupported_feature("qualified collection name"));
    };
    if identifier.value != expected_collection {
        return Err(invalid_input(format!(
            "collection '{}' does not match the active collection '{}'",
            identifier.value, expected_collection
        )));
    }
    Ok(identifier.value.clone())
}

fn parse_projection(
    projection: &[SelectItem],
    collection: &str,
    schema: &SchemaProfile,
) -> Result<Projection, ProxyError> {
    if projection.len() == 1 && matches!(projection.first(), Some(SelectItem::Wildcard(_))) {
        return Ok(Projection::All);
    }
    projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(expression) => Ok(ProjectedField {
                path: resolve_expression_field(expression, collection, schema)?,
                alias: None,
            }),
            SelectItem::ExprWithAlias { expr, alias } => Ok(ProjectedField {
                path: resolve_expression_field(expr, collection, schema)?,
                alias: Some(alias.value.clone()),
            }),
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => Err(
                unsupported_feature("mixed or qualified wildcard projection"),
            ),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Projection::Fields)
}

fn parse_predicate(
    expression: &Expr,
    collection: &str,
    schema: &SchemaProfile,
) -> Result<Predicate, ProxyError> {
    match expression {
        Expr::Nested(expression) => parse_predicate(expression, collection, schema),
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::And => Ok(Predicate::And(vec![
                parse_predicate(left, collection, schema)?,
                parse_predicate(right, collection, schema)?,
            ])),
            BinaryOperator::Or => Ok(Predicate::Or(vec![
                parse_predicate(left, collection, schema)?,
                parse_predicate(right, collection, schema)?,
            ])),
            _ => Ok(Predicate::Compare {
                path: resolve_expression_field(left, collection, schema)?,
                operator: parse_comparison_operator(op)?,
                value: parse_value(right)?,
            }),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Ok(Predicate::In {
            path: resolve_expression_field(expr, collection, schema)?,
            values: list
                .iter()
                .map(parse_value)
                .collect::<Result<Vec<_>, _>>()?,
            negated: *negated,
        }),
        Expr::IsNull(expression) => Ok(Predicate::IsNull {
            path: resolve_expression_field(expression, collection, schema)?,
            negated: false,
        }),
        Expr::IsNotNull(expression) => Ok(Predicate::IsNull {
            path: resolve_expression_field(expression, collection, schema)?,
            negated: true,
        }),
        _ => Err(unsupported_feature("WHERE expression")),
    }
}

fn parse_comparison_operator(operator: &BinaryOperator) -> Result<ComparisonOperator, ProxyError> {
    match operator {
        BinaryOperator::Eq => Ok(ComparisonOperator::Equal),
        BinaryOperator::NotEq => Ok(ComparisonOperator::NotEqual),
        BinaryOperator::Gt => Ok(ComparisonOperator::GreaterThan),
        BinaryOperator::GtEq => Ok(ComparisonOperator::GreaterThanOrEqual),
        BinaryOperator::Lt => Ok(ComparisonOperator::LessThan),
        BinaryOperator::LtEq => Ok(ComparisonOperator::LessThanOrEqual),
        _ => Err(unsupported_feature("comparison operator")),
    }
}

fn parse_limit(expression: &Expr) -> Result<u64, ProxyError> {
    let SqlValue::Integer(limit) = parse_value(expression)? else {
        return Err(invalid_input(
            "LIMIT must be a non-negative integer literal",
        ));
    };
    u64::try_from(limit).map_err(|_| invalid_input("LIMIT must be non-negative"))
}

fn parse_value(expression: &Expr) -> Result<SqlValue, ProxyError> {
    let Expr::Value(value) = expression else {
        return Err(unsupported_feature("non-literal value expression"));
    };
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::Boolean(value) => Ok(SqlValue::Boolean(*value)),
        Value::SingleQuotedString(value)
        | Value::EscapedStringLiteral(value)
        | Value::UnicodeStringLiteral(value)
        | Value::NationalStringLiteral(value) => Ok(SqlValue::String(value.clone())),
        Value::Placeholder(value) => Ok(SqlValue::Placeholder(value.clone())),
        Value::Number(value, _) => {
            if value.contains(['.', 'e', 'E']) {
                value.parse().map(SqlValue::FloatingPoint).map_err(|_| {
                    invalid_input(format!("numeric value is outside supported range: {value}"))
                })
            } else {
                value.parse().map(SqlValue::Integer).map_err(|_| {
                    invalid_input(format!("integer value is outside supported range: {value}"))
                })
            }
        }
        _ => Err(unsupported_feature("SQL literal type")),
    }
}

fn resolve_expression_field(
    expression: &Expr,
    collection: &str,
    schema: &SchemaProfile,
) -> Result<FieldPath, ProxyError> {
    match expression {
        Expr::Identifier(identifier) => {
            resolve_field(std::slice::from_ref(identifier), collection, schema)
        }
        Expr::CompoundIdentifier(identifiers) => resolve_field(identifiers, collection, schema),
        _ => Err(unsupported_feature("field expression")),
    }
}

fn resolve_field(
    identifiers: &[Ident],
    collection: &str,
    schema: &SchemaProfile,
) -> Result<FieldPath, ProxyError> {
    let identifiers = identifiers
        .strip_prefix(&[Ident::new(collection)])
        .unwrap_or(identifiers);
    let Some((first, rest)) = identifiers.split_first() else {
        return Err(invalid_input("field path cannot be empty"));
    };
    let path = rest
        .iter()
        .fold(FieldPath::top_level(&first.value), |path, part| {
            path.child(&part.value)
        });
    if schema.field(&path).is_none() {
        return Err(invalid_input(format!(
            "field '{}' is not present in the active schema profile",
            path.display_name()
        )));
    }
    Ok(path)
}

/// Resolves one `INSERT` column name.
///
/// The `PostgreSQL` grammar represents an `INSERT` column list as individual
/// identifiers, unlike SELECT expressions and UPDATE assignment targets. A
/// nested `MongoDB` path therefore arrives as one quoted identifier (for
/// example, `"profile.address.city"`). Interpret that form as segments only
/// when it matches a discovered nested path; an exact literal dotted key still
/// wins when it is the only matching field.
fn resolve_insert_column(
    identifier: &Ident,
    collection: &str,
    schema: &SchemaProfile,
) -> Result<FieldPath, ProxyError> {
    let exact = FieldPath::top_level(&identifier.value);
    if schema.field(&exact).is_some() {
        return Ok(exact);
    }
    let segments = identifier.value.split('.').collect::<Vec<_>>();
    let Some((first, rest)) = segments.split_first() else {
        return Err(invalid_input("field path cannot be empty"));
    };
    let path = rest
        .iter()
        .fold(FieldPath::top_level(*first), |path, segment| {
            path.child(*segment)
        });
    if schema.field(&path).is_some() {
        return Ok(path);
    }
    resolve_field(std::slice::from_ref(identifier), collection, schema)
}

fn invalid_input(message: impl Into<String>) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidInput, message)
}

/// Constructs the standard error used for SQL features outside the MVP.
#[must_use]
pub fn unsupported_feature(feature: &str) -> ProxyError {
    ProxyError::new(
        ErrorKind::FeatureNotSupported,
        format!("SQL feature is not supported: {feature}"),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mongo_pg_common::ErrorKind;
    use mongo_pg_schema_discovery::{FieldPath, FieldProfile, ObservedShape, ObservedType};

    use super::{ComparisonOperator, Predicate, Projection, SqlValue, StatementPlan, parse_sql};

    fn schema() -> mongo_pg_schema_discovery::SchemaProfile {
        let fields = [
            (FieldPath::top_level("name"), ObservedType::String),
            (FieldPath::top_level("active"), ObservedType::Boolean),
            (FieldPath::top_level("status"), ObservedType::Integer),
            (
                FieldPath::top_level("profile").child("city"),
                ObservedType::String,
            ),
        ]
        .into_iter()
        .map(|(path, observed_type)| FieldProfile {
            path,
            present_documents: 2,
            missing_documents: 0,
            observed_types: BTreeSet::from([observed_type]),
            observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
            has_dotted_key_collision: false,
        })
        .collect();
        mongo_pg_schema_discovery::SchemaProfile {
            profile_version: 1,
            sampled_documents: 2,
            fields,
        }
    }

    #[test]
    fn lowers_a_nested_select_predicate_to_a_typed_plan() {
        let plan = parse_sql(
            "SELECT name, profile.city FROM customers WHERE active = true AND status IN (1, 2) LIMIT 5",
            "customers",
            &schema(),
        )
        .expect("SELECT should parse");

        let StatementPlan::Select(plan) = plan else {
            panic!("expected SELECT plan");
        };
        assert_eq!(plan.limit, Some(5));
        assert!(matches!(plan.projection, Projection::Fields(fields) if fields.len() == 2));
        assert!(matches!(
            plan.filter,
            Some(Predicate::And(predicates)) if predicates.len() == 2
        ));
    }

    #[test]
    fn lowers_insert_update_and_delete_with_typed_values() {
        let insert = parse_sql(
            "INSERT INTO customers (name, active) VALUES ('Amina', true), ('Tendai', false)",
            "customers",
            &schema(),
        )
        .expect("INSERT should parse");
        assert!(matches!(
            insert,
            StatementPlan::Insert(plan) if plan.rows.len() == 2 && plan.rows[0][0] == SqlValue::String("Amina".into())
        ));

        let nested_insert = parse_sql(
            "INSERT INTO customers (name, \"profile.city\") VALUES ('Amina', 'Harare')",
            "customers",
            &schema(),
        )
        .expect("quoted nested INSERT column should parse");
        assert!(matches!(
            nested_insert,
            StatementPlan::Insert(plan) if plan.columns[1] == FieldPath::top_level("profile").child("city")
        ));

        let update = parse_sql(
            "UPDATE customers SET profile.city = 'Bulawayo' WHERE name = 'Amina'",
            "customers",
            &schema(),
        )
        .expect("UPDATE should parse");
        assert!(matches!(
            update,
            StatementPlan::Update(plan)
                if plan.assignments.len() == 1
                    && matches!(plan.filter, Predicate::Compare { operator: ComparisonOperator::Equal, .. })
        ));

        let delete = parse_sql(
            "DELETE FROM customers WHERE status >= 2",
            "customers",
            &schema(),
        )
        .expect("DELETE should parse");
        assert!(matches!(delete, StatementPlan::Delete(_)));
    }

    #[test]
    fn rejects_unsafe_or_unsupported_sql_before_execution() {
        let join_error = parse_sql(
            "SELECT * FROM customers JOIN orders ON customers.name = orders.customer",
            "customers",
            &schema(),
        )
        .expect_err("JOIN should be rejected");
        assert_eq!(join_error.kind, ErrorKind::FeatureNotSupported);

        let unfiltered_error = parse_sql("DELETE FROM customers", "customers", &schema())
            .expect_err("unfiltered DELETE should be rejected");
        assert_eq!(unfiltered_error.kind, ErrorKind::InvalidInput);

        let unknown_field_error = parse_sql(
            "SELECT unknown_field FROM customers",
            "customers",
            &schema(),
        )
        .expect_err("unknown field should be rejected");
        assert_eq!(unknown_field_error.kind, ErrorKind::InvalidInput);
    }
}
