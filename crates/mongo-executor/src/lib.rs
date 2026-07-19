//! Deterministic `MongoDB` execution boundary.
//!
//! This crate accepts typed SQL plans and creates structured driver operations.
//! It never accepts raw SQL text or LLM-generated `MongoDB` commands.

use futures_util::TryStreamExt;
use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_sql_engine::{
    ComparisonOperator, Predicate, Projection, SelectPlan, SqlValue, StatementPlan,
};
use mongodb::{
    Database,
    bson::{Bson, Document},
};

/// Counts returned from a completed `MongoDB` write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    pub matched: u64,
    pub modified: u64,
    pub inserted: u64,
    pub deleted: u64,
}

/// Documents returned from a deterministic read execution.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectOutcome {
    pub documents: Vec<Document>,
}

/// Executes a typed `SELECT` plan through a `MongoDB` `find` operation.
///
/// # Errors
///
/// Returns an error when plan values cannot be represented safely in BSON or
/// when the `MongoDB` driver rejects the operation.
pub async fn execute_select(
    database: &Database,
    plan: &SelectPlan,
) -> Result<SelectOutcome, ProxyError> {
    let filter = filter_document(plan.filter.as_ref())?;
    let projection = projection_document(&plan.projection)?;
    let limit = plan.limit.map(|value| {
        i64::try_from(value)
            .map_err(|_| invalid_input("SELECT limit exceeds the MongoDB driver range"))
    });
    let limit = limit.transpose()?;
    let collection = database.collection::<Document>(&plan.collection);

    let cursor = match (projection, limit) {
        (Some(projection), Some(limit)) => {
            collection
                .find(filter)
                .projection(projection)
                .limit(limit)
                .await
        }
        (Some(projection), None) => collection.find(filter).projection(projection).await,
        (None, Some(limit)) => collection.find(filter).limit(limit).await,
        (None, None) => collection.find(filter).await,
    }
    .map_err(|error| database_error(&error))?;
    let documents = cursor
        .try_collect()
        .await
        .map_err(|error| database_error(&error))?;

    Ok(SelectOutcome { documents })
}

/// Refuses plans other than reads until their dedicated deterministic executor
/// implementation is complete.
///
/// # Errors
///
/// Returns an unsupported-feature error for plans other than `SELECT`.
pub async fn execute_read_only(
    database: &Database,
    plan: &StatementPlan,
) -> Result<SelectOutcome, ProxyError> {
    let StatementPlan::Select(select) = plan else {
        return Err(ProxyError::new(
            ErrorKind::FeatureNotSupported,
            "write execution is not implemented yet",
        ));
    };
    execute_select(database, select).await
}

fn filter_document(predicate: Option<&Predicate>) -> Result<Document, ProxyError> {
    Ok(predicate
        .map(predicate_document)
        .transpose()?
        .unwrap_or_default())
}

fn predicate_document(predicate: &Predicate) -> Result<Document, ProxyError> {
    match predicate {
        Predicate::Compare {
            path,
            operator,
            value,
        } => {
            let field = mongo_field_name(path)?;
            let value = bson_value(value)?;
            match operator {
                ComparisonOperator::Equal => Ok(single_field(&field, value)),
                ComparisonOperator::NotEqual => Ok(operator_field(&field, "$ne", value)),
                ComparisonOperator::GreaterThan => Ok(operator_field(&field, "$gt", value)),
                ComparisonOperator::GreaterThanOrEqual => Ok(operator_field(&field, "$gte", value)),
                ComparisonOperator::LessThan => Ok(operator_field(&field, "$lt", value)),
                ComparisonOperator::LessThanOrEqual => Ok(operator_field(&field, "$lte", value)),
            }
        }
        Predicate::In {
            path,
            values,
            negated,
        } => {
            let field = mongo_field_name(path)?;
            let values = values
                .iter()
                .map(bson_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(operator_field(
                &field,
                if *negated { "$nin" } else { "$in" },
                Bson::Array(values),
            ))
        }
        Predicate::IsNull { path, negated } => {
            let field = mongo_field_name(path)?;
            let null_check = if *negated {
                operator_field(&field, "$ne", Bson::Null)
            } else {
                single_field(&field, Bson::Null)
            };
            let exists_check = operator_field(&field, "$exists", Bson::Boolean(true));
            logical_document("$and", vec![null_check, exists_check])
        }
        Predicate::And(predicates) => logical_document(
            "$and",
            predicates
                .iter()
                .map(predicate_document)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Predicate::Or(predicates) => logical_document(
            "$or",
            predicates
                .iter()
                .map(predicate_document)
                .collect::<Result<Vec<_>, _>>()?,
        ),
    }
}

fn projection_document(projection: &Projection) -> Result<Option<Document>, ProxyError> {
    match projection {
        Projection::All => Ok(None),
        Projection::Fields(fields) => {
            let mut document = Document::new();
            for field in fields {
                document.insert(mongo_field_name(&field.path)?, 1_i32);
            }
            Ok(Some(document))
        }
    }
}

fn mongo_field_name(path: &mongo_pg_schema_discovery::FieldPath) -> Result<String, ProxyError> {
    if path.is_literal_dotted_key() {
        return Err(ProxyError::new(
            ErrorKind::FeatureNotSupported,
            "literal dotted MongoDB field names require an aggregation implementation",
        ));
    }
    Ok(path.display_name())
}

fn bson_value(value: &SqlValue) -> Result<Bson, ProxyError> {
    match value {
        SqlValue::Null => Ok(Bson::Null),
        SqlValue::Boolean(value) => Ok(Bson::Boolean(*value)),
        SqlValue::Integer(value) => Ok(Bson::Int64(*value)),
        SqlValue::FloatingPoint(value) => Ok(Bson::Double(*value)),
        SqlValue::String(value) => Ok(Bson::String(value.clone())),
        SqlValue::Placeholder(value) => Err(invalid_input(format!(
            "parameter '{value}' is not bound; prepared-statement binding is not implemented"
        ))),
    }
}

fn single_field(field: &str, value: Bson) -> Document {
    let mut document = Document::new();
    document.insert(field, value);
    document
}

fn operator_field(field: &str, operator: &str, value: Bson) -> Document {
    let mut operators = Document::new();
    operators.insert(operator, value);
    single_field(field, Bson::Document(operators))
}

fn logical_document(operator: &str, documents: Vec<Document>) -> Result<Document, ProxyError> {
    if documents.is_empty() {
        return Err(invalid_input(
            "logical predicates must contain at least one condition",
        ));
    }
    Ok(single_field(
        operator,
        Bson::Array(documents.into_iter().map(Bson::Document).collect()),
    ))
}

fn invalid_input(message: impl Into<String>) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidInput, message)
}

fn database_error(error: &mongodb::error::Error) -> ProxyError {
    ProxyError::new(
        ErrorKind::Database,
        format!("MongoDB operation failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use mongo_pg_common::ErrorKind;
    use mongo_pg_schema_discovery::FieldPath;
    use mongo_pg_sql_engine::{ComparisonOperator, Predicate, SqlValue};
    use mongodb::bson::{Bson, doc};

    use super::predicate_document;

    #[test]
    fn builds_nested_comparison_filters() {
        let filter = predicate_document(&Predicate::Compare {
            path: FieldPath::top_level("profile").child("city"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("Harare".into()),
        })
        .expect("nested path should be executable");
        assert_eq!(filter, doc! { "profile.city": "Harare" });
    }

    #[test]
    fn keeps_sql_null_checks_distinct_from_missing_fields() {
        let filter = predicate_document(&Predicate::IsNull {
            path: FieldPath::top_level("status"),
            negated: false,
        })
        .expect("null predicate should be executable");
        let conditions = filter
            .get_array("$and")
            .expect("null predicate should use an AND condition");
        assert_eq!(conditions.len(), 2);
        assert!(matches!(conditions[0], Bson::Document(_)));
    }

    #[test]
    fn refuses_literal_dotted_keys_in_find_queries() {
        let error = predicate_document(&Predicate::Compare {
            path: FieldPath::top_level("profile.city"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("literal".into()),
        })
        .expect_err("literal dotted keys require an aggregation path");
        assert_eq!(error.kind, ErrorKind::FeatureNotSupported);
    }
}
