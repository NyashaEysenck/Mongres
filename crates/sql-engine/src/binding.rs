//! Prepared-statement placeholder binding for deterministic SQL plans.
//!
//! The wire-protocol adapter decodes `PostgreSQL` parameter bytes into
//! [`SqlValue`] values. This module only substitutes those typed values into a
//! plan and applies the same schema validation used for SQL literals.

use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_schema_discovery::SchemaProfile;

use crate::{
    AssignmentPlan, DeletePlan, InsertPlan, Predicate, SelectPlan, SqlValue, StatementPlan,
    UpdatePlan,
    type_policy::{validate_read_predicate, validate_write_predicate, validate_write_value},
};

/// Binds `PostgreSQL` positional parameters to a parsed SQL plan.
///
/// Placeholders must use `PostgreSQL`'s one-based `$n` syntax. The supplied
/// values must have exactly the number of entries required by the largest
/// placeholder index, and bound values cannot themselves be placeholders.
///
/// # Errors
///
/// Returns an invalid-input error for malformed, missing, or extra parameters,
/// or when a bound value violates the active schema's deterministic type rules.
pub fn bind_parameters(
    plan: StatementPlan,
    parameters: &[SqlValue],
    schema: &SchemaProfile,
) -> Result<StatementPlan, ProxyError> {
    if parameters
        .iter()
        .any(|value| matches!(value, SqlValue::Placeholder(_)))
    {
        return Err(invalid_input(
            "bound parameter values cannot be placeholders",
        ));
    }

    let required_count = maximum_placeholder_index(&plan)?;
    if required_count != parameters.len() {
        return Err(invalid_input(format!(
            "prepared statement requires {required_count} parameters but received {}",
            parameters.len()
        )));
    }

    let plan = match plan {
        StatementPlan::Select(plan) => StatementPlan::Select(SelectPlan {
            collection: plan.collection,
            projection: plan.projection,
            filter: plan
                .filter
                .map(|predicate| bind_predicate(predicate, parameters))
                .transpose()?,
            limit: plan.limit,
        }),
        StatementPlan::Insert(plan) => StatementPlan::Insert(InsertPlan {
            collection: plan.collection,
            columns: plan.columns,
            rows: plan
                .rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|value| bind_value(value, parameters))
                        .collect()
                })
                .collect::<Result<_, _>>()?,
        }),
        StatementPlan::Update(plan) => StatementPlan::Update(UpdatePlan {
            collection: plan.collection,
            assignments: plan
                .assignments
                .into_iter()
                .map(|assignment| {
                    Ok(AssignmentPlan {
                        path: assignment.path,
                        value: bind_value(assignment.value, parameters)?,
                    })
                })
                .collect::<Result<_, ProxyError>>()?,
            filter: bind_predicate(plan.filter, parameters)?,
        }),
        StatementPlan::Delete(plan) => StatementPlan::Delete(DeletePlan {
            collection: plan.collection,
            filter: bind_predicate(plan.filter, parameters)?,
        }),
    };
    validate_bound_plan(&plan, schema)?;
    Ok(plan)
}

fn maximum_placeholder_index(plan: &StatementPlan) -> Result<usize, ProxyError> {
    let mut maximum = 0;
    match plan {
        StatementPlan::Select(plan) => {
            if let Some(predicate) = &plan.filter {
                maximum = maximum.max(maximum_predicate_index(predicate)?);
            }
        }
        StatementPlan::Insert(plan) => {
            for row in &plan.rows {
                for value in row {
                    maximum = maximum.max(placeholder_index(value)?);
                }
            }
        }
        StatementPlan::Update(plan) => {
            for assignment in &plan.assignments {
                maximum = maximum.max(placeholder_index(&assignment.value)?);
            }
            maximum = maximum.max(maximum_predicate_index(&plan.filter)?);
        }
        StatementPlan::Delete(plan) => maximum = maximum_predicate_index(&plan.filter)?,
    }
    Ok(maximum)
}

fn maximum_predicate_index(predicate: &Predicate) -> Result<usize, ProxyError> {
    match predicate {
        Predicate::Compare { value, .. } => placeholder_index(value),
        Predicate::In { values, .. } => values.iter().try_fold(0, |maximum, value| {
            Ok(maximum.max(placeholder_index(value)?))
        }),
        Predicate::IsNull { .. } => Ok(0),
        Predicate::And(predicates) | Predicate::Or(predicates) => {
            predicates.iter().try_fold(0, |maximum, predicate| {
                Ok(maximum.max(maximum_predicate_index(predicate)?))
            })
        }
    }
}

fn bind_predicate(predicate: Predicate, parameters: &[SqlValue]) -> Result<Predicate, ProxyError> {
    match predicate {
        Predicate::Compare {
            path,
            operator,
            value,
        } => Ok(Predicate::Compare {
            path,
            operator,
            value: bind_value(value, parameters)?,
        }),
        Predicate::In {
            path,
            values,
            negated,
        } => Ok(Predicate::In {
            path,
            values: values
                .into_iter()
                .map(|value| bind_value(value, parameters))
                .collect::<Result<_, _>>()?,
            negated,
        }),
        Predicate::IsNull { path, negated } => Ok(Predicate::IsNull { path, negated }),
        Predicate::And(predicates) => Ok(Predicate::And(
            predicates
                .into_iter()
                .map(|predicate| bind_predicate(predicate, parameters))
                .collect::<Result<_, _>>()?,
        )),
        Predicate::Or(predicates) => Ok(Predicate::Or(
            predicates
                .into_iter()
                .map(|predicate| bind_predicate(predicate, parameters))
                .collect::<Result<_, _>>()?,
        )),
    }
}

fn bind_value(value: SqlValue, parameters: &[SqlValue]) -> Result<SqlValue, ProxyError> {
    let SqlValue::Placeholder(placeholder) = value else {
        return Ok(value);
    };
    let index = parse_placeholder_index(&placeholder)?;
    parameters
        .get(index - 1)
        .cloned()
        .ok_or_else(|| invalid_input(format!("missing bound value for placeholder {placeholder}")))
}

fn placeholder_index(value: &SqlValue) -> Result<usize, ProxyError> {
    match value {
        SqlValue::Placeholder(placeholder) => parse_placeholder_index(placeholder),
        _ => Ok(0),
    }
}

fn parse_placeholder_index(placeholder: &str) -> Result<usize, ProxyError> {
    let Some(index) = placeholder.strip_prefix('$') else {
        return Err(invalid_input(format!(
            "unsupported prepared-statement placeholder '{placeholder}'; use PostgreSQL $n placeholders"
        )));
    };
    let index = index.parse::<usize>().map_err(|_| {
        invalid_input(format!(
            "invalid prepared-statement placeholder '{placeholder}'; expected $ followed by a positive integer"
        ))
    })?;
    if index == 0 {
        return Err(invalid_input(
            "prepared-statement placeholder indexes start at $1",
        ));
    }
    Ok(index)
}

fn validate_bound_plan(plan: &StatementPlan, schema: &SchemaProfile) -> Result<(), ProxyError> {
    match plan {
        StatementPlan::Select(plan) => plan
            .filter
            .as_ref()
            .map(|predicate| validate_read_predicate(predicate, schema))
            .transpose()
            .map(|_| ()),
        StatementPlan::Insert(plan) => plan.rows.iter().try_for_each(|row| {
            plan.columns
                .iter()
                .zip(row)
                .try_for_each(|(path, value)| validate_write_value(path, value, schema))
        }),
        StatementPlan::Update(plan) => {
            plan.assignments.iter().try_for_each(|assignment| {
                validate_write_value(&assignment.path, &assignment.value, schema)
            })?;
            validate_write_predicate(&plan.filter, schema)
        }
        StatementPlan::Delete(plan) => validate_write_predicate(&plan.filter, schema),
    }
}

fn invalid_input(message: impl Into<String>) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidInput, message)
}
