//! Expression, literal, collection, and field-path lowering helpers.

use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_schema_discovery::{FieldPath, SchemaProfile};
use sqlparser::ast::{BinaryOperator, Expr, Ident, ObjectName, TableFactor, TableWithJoins, Value};

use crate::{ComparisonOperator, Predicate, SqlValue, unsupported_feature};

pub(crate) fn collection_from_table_with_joins(
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

pub(crate) fn collection_from_object_name(
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

pub(crate) fn parse_predicate(
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

pub(crate) fn parse_limit(expression: &Expr) -> Result<u64, ProxyError> {
    let SqlValue::Integer(limit) = parse_value(expression)? else {
        return Err(invalid_input(
            "LIMIT must be a non-negative integer literal",
        ));
    };
    u64::try_from(limit).map_err(|_| invalid_input("LIMIT must be non-negative"))
}

pub(crate) fn parse_value(expression: &Expr) -> Result<SqlValue, ProxyError> {
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

pub(crate) fn resolve_expression_field(
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

pub(crate) fn resolve_field(
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
pub(crate) fn resolve_insert_column(
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

fn invalid_input(message: impl Into<String>) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidInput, message)
}
