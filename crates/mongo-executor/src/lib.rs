//! Deterministic `MongoDB` execution boundary.
//!
//! This crate accepts typed SQL plans and creates structured driver operations.
//! It never accepts raw SQL text or LLM-generated `MongoDB` commands.

use futures_util::TryStreamExt;
use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_sql_engine::{
    AssignmentPlan, ComparisonOperator, DeletePlan, InsertPlan, Predicate, Projection, SelectPlan,
    SqlValue, StatementPlan, UpdatePlan,
};
use mongodb::{
    Client, Database,
    bson::{Bson, Document},
    options::{ClientOptions, WriteConcern},
};

/// Counts returned from a completed `MongoDB` write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    pub matched: u64,
    pub modified: u64,
    pub inserted: u64,
    pub deleted: u64,
}

/// Applies the required policy for deterministic proxy writes.
///
/// The proxy does not reissue writes itself. Driver retryable writes are
/// disabled so a network ambiguity is returned to the caller rather than
/// silently re-executing a potentially non-idempotent statement.
pub fn apply_deterministic_write_policy(options: &mut ClientOptions) {
    options.retry_writes = Some(false);
    options.write_concern = Some(WriteConcern::majority());
}

/// Creates a `MongoDB` client configured for deterministic proxy writes.
///
/// # Errors
///
/// Returns a dependency error when the connection string cannot be parsed or
/// the configured client cannot be created.
pub async fn deterministic_write_client(uri: &str) -> Result<Client, ProxyError> {
    let mut options = ClientOptions::parse(uri)
        .await
        .map_err(|error| client_configuration_error(&error))?;
    apply_deterministic_write_policy(&mut options);
    Client::with_options(options).map_err(|error| client_configuration_error(&error))
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

/// Executes a typed `INSERT` plan with deterministic BSON document creation.
///
/// # Errors
///
/// Returns an error when a row does not match its columns, a field path
/// conflicts with another assigned path, a value is unsafe to represent, or
/// the `MongoDB` driver rejects the insert.
pub async fn execute_insert(
    database: &Database,
    plan: &InsertPlan,
) -> Result<WriteOutcome, ProxyError> {
    let documents = plan
        .rows
        .iter()
        .map(|row| build_insert_document(&plan.columns, row))
        .collect::<Result<Vec<_>, _>>()?;
    if documents.is_empty() {
        return Err(invalid_input("INSERT requires at least one row"));
    }
    let collection = database.collection::<Document>(&plan.collection);

    let inserted = if documents.len() == 1 {
        collection
            .insert_one(
                documents
                    .into_iter()
                    .next()
                    .ok_or_else(|| invalid_input("INSERT requires at least one row"))?,
            )
            .await
            .map_err(|error| write_error(&error))?;
        1
    } else {
        collection
            .insert_many(documents)
            .await
            .map_err(|error| write_error(&error))?
            .inserted_ids
            .len() as u64
    };

    Ok(WriteOutcome {
        inserted,
        ..WriteOutcome::default()
    })
}

/// Executes a typed `UPDATE` plan with a deterministic `$set` operation.
///
/// Every matching document is updated, mirroring SQL `UPDATE` semantics. The
/// assignment document is constructed only from validated path segments; it
/// cannot contain overlapping paths, literal dotted keys, or unbound values.
///
/// # Errors
///
/// Returns an error when the filter or assignments cannot be represented
/// safely, or when the `MongoDB` driver rejects the operation.
pub async fn execute_update(
    database: &Database,
    plan: &UpdatePlan,
) -> Result<WriteOutcome, ProxyError> {
    let filter = predicate_document(&plan.filter)?;
    let update = update_document(&plan.assignments)?;
    let collection = database.collection::<Document>(&plan.collection);
    let result = collection
        .update_many(filter, update)
        .await
        .map_err(|error| write_error(&error))?;

    Ok(WriteOutcome {
        matched: result.matched_count,
        modified: result.modified_count,
        ..WriteOutcome::default()
    })
}

/// Executes a typed `DELETE` plan against every matching document.
///
/// The SQL layer requires a `WHERE` clause before producing a `DeletePlan`;
/// this executor still translates only the typed predicate rather than
/// accepting a caller-supplied `MongoDB` filter.
///
/// # Errors
///
/// Returns an error when the filter cannot be represented safely or when the
/// `MongoDB` driver rejects the operation.
pub async fn execute_delete(
    database: &Database,
    plan: &DeletePlan,
) -> Result<WriteOutcome, ProxyError> {
    let filter = predicate_document(&plan.filter)?;
    let collection = database.collection::<Document>(&plan.collection);
    let result = collection
        .delete_many(filter)
        .await
        .map_err(|error| write_error(&error))?;

    Ok(WriteOutcome {
        deleted: result.deleted_count,
        ..WriteOutcome::default()
    })
}

/// Executes a typed write plan without applying application-level retries.
///
/// # Errors
///
/// Returns an unsupported-feature error for `SELECT` plans and forwards the
/// appropriate deterministic write error for write plans.
pub async fn execute_write(
    database: &Database,
    plan: &StatementPlan,
) -> Result<WriteOutcome, ProxyError> {
    match plan {
        StatementPlan::Insert(insert) => execute_insert(database, insert).await,
        StatementPlan::Update(update) => execute_update(database, update).await,
        StatementPlan::Delete(delete) => execute_delete(database, delete).await,
        StatementPlan::Select(_) => Err(ProxyError::new(
            ErrorKind::FeatureNotSupported,
            "SELECT plans must use the read executor",
        )),
    }
}

/// Executes only read plans for callers that intentionally expose a
/// read-only surface.
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

fn build_insert_document(
    columns: &[mongo_pg_schema_discovery::FieldPath],
    row: &[SqlValue],
) -> Result<Document, ProxyError> {
    if columns.len() != row.len() {
        return Err(invalid_input(
            "INSERT row must contain one value for each declared column",
        ));
    }

    let mut document = Document::new();
    for (path, value) in columns.iter().zip(row) {
        if path.is_literal_dotted_key() {
            return Err(ProxyError::new(
                ErrorKind::FeatureNotSupported,
                "literal dotted MongoDB field names require an aggregation implementation",
            ));
        }
        insert_nested_value(&mut document, path.segments(), bson_value(value)?)?;
    }
    Ok(document)
}

fn update_document(assignments: &[AssignmentPlan]) -> Result<Document, ProxyError> {
    if assignments.is_empty() {
        return Err(invalid_input("UPDATE requires at least one assignment"));
    }

    let mut set = Document::new();
    let mut assigned_paths = Vec::with_capacity(assignments.len());
    for assignment in assignments {
        let field = mongo_field_name(&assignment.path)?;
        validate_update_path(&assignment.path, &assigned_paths)?;
        set.insert(field, bson_value(&assignment.value)?);
        assigned_paths.push(assignment.path.segments());
    }

    Ok(single_field("$set", Bson::Document(set)))
}

fn validate_update_path(
    path: &mongo_pg_schema_discovery::FieldPath,
    assigned_paths: &[&[String]],
) -> Result<(), ProxyError> {
    let segments = path.segments();
    if segments.is_empty() || segments.iter().any(String::is_empty) {
        return Err(invalid_input(
            "UPDATE field path cannot contain empty segments",
        ));
    }
    if segments.iter().any(|segment| segment.starts_with('$')) {
        return Err(invalid_input(
            "UPDATE field path cannot contain MongoDB operator-like segments",
        ));
    }
    if assigned_paths
        .iter()
        .any(|other| paths_overlap(segments, other))
    {
        return Err(invalid_input(format!(
            "UPDATE assignment path '{}' conflicts with another assignment path",
            path.display_name()
        )));
    }
    Ok(())
}

fn paths_overlap(left: &[String], right: &[String]) -> bool {
    let common_length = left.len().min(right.len());
    left[..common_length] == right[..common_length]
}

fn insert_nested_value(
    document: &mut Document,
    segments: &[String],
    value: Bson,
) -> Result<(), ProxyError> {
    let Some((field, remaining)) = segments.split_first() else {
        return Err(invalid_input("INSERT field path cannot be empty"));
    };
    if remaining.is_empty() {
        if document.contains_key(field) {
            return Err(invalid_input(format!(
                "INSERT assigns field '{field}' more than once"
            )));
        }
        document.insert(field, value);
        return Ok(());
    }

    match document.get_mut(field) {
        Some(Bson::Document(nested)) => insert_nested_value(nested, remaining, value),
        Some(_) => Err(invalid_input(format!(
            "INSERT field path conflicts with non-document field '{field}'"
        ))),
        None => {
            let mut nested = Document::new();
            insert_nested_value(&mut nested, remaining, value)?;
            document.insert(field, Bson::Document(nested));
            Ok(())
        }
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

fn write_error(error: &mongodb::error::Error) -> ProxyError {
    ProxyError::new(
        ErrorKind::Database,
        format!(
            "MongoDB write failed and may have partially applied; inspect the database before retrying: {error}"
        ),
    )
}

fn client_configuration_error(error: &mongodb::error::Error) -> ProxyError {
    ProxyError::new(
        ErrorKind::Dependency,
        format!("MongoDB client configuration failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use mongo_pg_common::ErrorKind;
    use mongo_pg_schema_discovery::FieldPath;
    use mongo_pg_sql_engine::{AssignmentPlan, ComparisonOperator, Predicate, SqlValue};
    use mongodb::{
        bson::{Bson, doc},
        options::{ClientOptions, WriteConcern},
    };

    use super::{
        apply_deterministic_write_policy, build_insert_document, predicate_document,
        update_document,
    };

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

    #[test]
    fn builds_nested_documents_for_insert_rows() {
        let document = build_insert_document(
            &[
                FieldPath::top_level("name"),
                FieldPath::top_level("profile").child("city"),
            ],
            &[
                SqlValue::String("Amina".into()),
                SqlValue::String("Harare".into()),
            ],
        )
        .expect("nested INSERT row should be valid");
        assert_eq!(document.get_str("name"), Ok("Amina"));
        assert_eq!(
            document
                .get_document("profile")
                .and_then(|profile| profile.get_str("city")),
            Ok("Harare")
        );
    }

    #[test]
    fn rejects_conflicting_insert_paths() {
        let error = build_insert_document(
            &[
                FieldPath::top_level("profile"),
                FieldPath::top_level("profile").child("city"),
            ],
            &[
                SqlValue::String("not-an-object".into()),
                SqlValue::String("Harare".into()),
            ],
        )
        .expect_err("a scalar and its nested path cannot be inserted together");
        assert_eq!(error.kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn builds_a_safe_nested_set_update() {
        let update = update_document(&[
            AssignmentPlan {
                path: FieldPath::top_level("profile").child("city"),
                value: SqlValue::String("Harare".into()),
            },
            AssignmentPlan {
                path: FieldPath::top_level("status"),
                value: SqlValue::String("active".into()),
            },
        ])
        .expect("non-overlapping paths should form a $set document");

        assert_eq!(
            update,
            doc! { "$set": { "profile.city": "Harare", "status": "active" } }
        );
    }

    #[test]
    fn rejects_overlapping_update_paths() {
        let error = update_document(&[
            AssignmentPlan {
                path: FieldPath::top_level("profile"),
                value: SqlValue::String("not-an-object".into()),
            },
            AssignmentPlan {
                path: FieldPath::top_level("profile").child("city"),
                value: SqlValue::String("Harare".into()),
            },
        ])
        .expect_err("overlapping assignments must not be sent to MongoDB");
        assert_eq!(error.kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_unbound_update_parameters() {
        let error = update_document(&[AssignmentPlan {
            path: FieldPath::top_level("status"),
            value: SqlValue::Placeholder("$1".into()),
        }])
        .expect_err("write execution cannot run with an unbound parameter");
        assert_eq!(error.kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn applies_an_acknowledged_no_retry_write_policy() {
        let mut options = ClientOptions::default();
        apply_deterministic_write_policy(&mut options);
        assert_eq!(options.retry_writes, Some(false));
        assert_eq!(options.write_concern, Some(WriteConcern::majority()));
    }
}
