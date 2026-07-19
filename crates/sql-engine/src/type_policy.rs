//! Schema-backed SQL-literal validation for deterministic `MongoDB` operations.

use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_schema_discovery::{FieldPath, ObservedType, SchemaProfile};

use crate::{Predicate, SqlValue};

/// Validates values used to select documents for a write.
///
/// A type or shape ambiguity must be resolved before a write can use it as a
/// filter, because changing the filter's BSON type can change which documents
/// are modified or deleted.
pub(crate) fn validate_write_predicate(
    predicate: &Predicate,
    schema: &SchemaProfile,
) -> Result<(), ProxyError> {
    match predicate {
        Predicate::Compare { path, value, .. } => validate_value(path, value, schema, false, true),
        Predicate::In { path, values, .. } => values
            .iter()
            .try_for_each(|value| validate_value(path, value, schema, false, true)),
        Predicate::IsNull { path, .. } => validate_field_shape(path, schema, true).map(|_| ()),
        Predicate::And(predicates) | Predicate::Or(predicates) => predicates
            .iter()
            .try_for_each(|predicate| validate_write_predicate(predicate, schema)),
    }
}

/// Validates a value assigned by `INSERT` or `UPDATE`.
///
/// SQL NULL is permitted for an assignment, but all non-null literals must
/// exactly match the one inferred BSON scalar type. The proxy intentionally
/// does not invent string-to-ObjectId, string-to-date, numeric widening, or
/// document/array coercions.
pub(crate) fn validate_write_value(
    path: &FieldPath,
    value: &SqlValue,
    schema: &SchemaProfile,
) -> Result<(), ProxyError> {
    validate_value(path, value, schema, true, true)
}

/// Validates values used in a read predicate.
///
/// Ambiguous fields are rejected as invalid input for reads; they are never
/// sent to the write-time resolver.
pub(crate) fn validate_read_predicate(
    predicate: &Predicate,
    schema: &SchemaProfile,
) -> Result<(), ProxyError> {
    match predicate {
        Predicate::Compare { path, value, .. } => validate_value(path, value, schema, false, false),
        Predicate::In { path, values, .. } => values
            .iter()
            .try_for_each(|value| validate_value(path, value, schema, false, false)),
        Predicate::IsNull { path, .. } => validate_field_shape(path, schema, false).map(|_| ()),
        Predicate::And(predicates) | Predicate::Or(predicates) => predicates
            .iter()
            .try_for_each(|predicate| validate_read_predicate(predicate, schema)),
    }
}

fn validate_value(
    path: &FieldPath,
    value: &SqlValue,
    schema: &SchemaProfile,
    null_is_allowed: bool,
    ambiguity_blocks_operation: bool,
) -> Result<(), ProxyError> {
    if matches!(value, SqlValue::Placeholder(_)) {
        // Typed protocol binding owns validation when placeholders become
        // executable. The executor currently rejects unbound placeholders.
        return Ok(());
    }
    if matches!(value, SqlValue::Null) {
        return if null_is_allowed {
            Ok(())
        } else {
            Err(invalid_input(
                "SQL NULL cannot be used in a comparison or IN list; use IS NULL or IS NOT NULL",
            ))
        };
    }

    let observed_type = validate_field_shape(path, schema, ambiguity_blocks_operation)?;
    if !is_compatible(value, observed_type) {
        return Err(invalid_input(format!(
            "value for field '{}' is not compatible with inferred BSON type '{}'",
            path.display_name(),
            observed_type_name(observed_type)
        )));
    }
    Ok(())
}

fn validate_field_shape(
    path: &FieldPath,
    schema: &SchemaProfile,
    ambiguity_blocks_operation: bool,
) -> Result<ObservedType, ProxyError> {
    let profile = schema.field(path).ok_or_else(|| {
        invalid_input(format!(
            "field '{}' is not present in the active schema profile",
            path.display_name()
        ))
    })?;
    let non_null_types = profile
        .observed_types
        .iter()
        .copied()
        .filter(|observed_type| *observed_type != ObservedType::Null)
        .collect::<Vec<_>>();

    if profile.has_dotted_key_collision
        || profile.observed_shapes.len() != 1
        || non_null_types.len() != 1
    {
        let error = format!(
            "field '{}' has an ambiguous inferred type or shape",
            path.display_name()
        );
        return if ambiguity_blocks_operation {
            Err(ProxyError::new(ErrorKind::AmbiguousWrite, error))
        } else {
            Err(invalid_input(error))
        };
    }
    Ok(non_null_types[0])
}

fn is_compatible(value: &SqlValue, observed_type: ObservedType) -> bool {
    matches!(
        (value, observed_type),
        (SqlValue::Boolean(_), ObservedType::Boolean)
            | (SqlValue::Integer(_), ObservedType::Integer)
            | (SqlValue::FloatingPoint(_), ObservedType::FloatingPoint)
            | (SqlValue::String(_), ObservedType::String)
    )
}

fn observed_type_name(observed_type: ObservedType) -> &'static str {
    match observed_type {
        ObservedType::Null => "null",
        ObservedType::Boolean => "boolean",
        ObservedType::Integer => "integer",
        ObservedType::FloatingPoint => "floating-point",
        ObservedType::String => "string",
        ObservedType::DateTime => "datetime",
        ObservedType::ObjectId => "object-id",
        ObservedType::Document => "document",
        ObservedType::Array => "array",
    }
}

fn invalid_input(message: impl Into<String>) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidInput, message)
}
