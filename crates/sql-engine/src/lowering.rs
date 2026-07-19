//! Statement-specific SQL validation and lowering.

use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_schema_discovery::SchemaProfile;
use sqlparser::{
    ast::{
        AssignmentTarget, Delete, FromTable, GroupByExpr, Insert, Select, SelectItem, SetExpr,
        Statement,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};

use crate::{
    AssignmentPlan, DeletePlan, InsertPlan, ProjectedField, Projection, SelectPlan, StatementPlan,
    UpdatePlan,
    resolve::{
        collection_from_object_name, collection_from_table_with_joins, parse_limit,
        parse_predicate, parse_value, resolve_expression_field, resolve_field,
        resolve_insert_column,
    },
    type_policy::{validate_read_predicate, validate_write_predicate, validate_write_value},
    unsupported_feature,
};

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
    if let Some(predicate) = &filter {
        validate_read_predicate(predicate, schema)?;
    }
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
            let values = row.iter().map(parse_value).collect::<Result<Vec<_>, _>>()?;
            columns
                .iter()
                .zip(&values)
                .try_for_each(|(path, value)| validate_write_value(path, value, schema))?;
            Ok(values)
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
    table: &sqlparser::ast::TableWithJoins,
    assignments: &[sqlparser::ast::Assignment],
    from: Option<&sqlparser::ast::TableWithJoins>,
    selection: Option<&sqlparser::ast::Expr>,
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
    for assignment in &assignments {
        validate_write_value(&assignment.path, &assignment.value, schema)?;
    }
    validate_write_predicate(&filter, schema)?;

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
    validate_write_predicate(&filter, schema)?;

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

fn invalid_input(message: impl Into<String>) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidInput, message)
}
