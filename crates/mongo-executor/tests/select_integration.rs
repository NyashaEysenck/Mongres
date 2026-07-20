//! Live coverage for deterministic `MongoDB` `find` execution.

use std::{collections::BTreeSet, env};

use mongo_pg_common::ErrorKind;
use mongo_pg_mongo_executor::{deterministic_write_client, execute_insert, execute_select};
use mongo_pg_schema_discovery::FieldPath;
use mongo_pg_sql_engine::{
    ComparisonOperator, InsertPlan, Predicate, ProjectedField, Projection, SelectPlan, SqlValue,
};
use mongodb::bson::{Document, doc};

const SELECT_TEST_COLLECTION: &str = "executor_select_integration";
const INSERT_TEST_COLLECTION: &str = "executor_insert_integration";
const INSERT_FAILURE_COLLECTION: &str = "executor_insert_failure_integration";
const PREDICATE_MATRIX_COLLECTION: &str = "executor_predicate_matrix_integration";
const PARTIAL_INSERT_COLLECTION: &str = "executor_partial_insert_integration";

fn projected_names(documents: &[Document]) -> BTreeSet<String> {
    documents
        .iter()
        .map(|document| {
            document
                .get_str("name")
                .expect("projection should include name")
                .to_owned()
        })
        .collect()
}

fn name_projection() -> Projection {
    Projection::Fields(vec![ProjectedField {
        path: FieldPath::top_level("name"),
        alias: None,
    }])
}

/// Requires a running local `MongoDB` instance.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn executes_a_nested_filter_and_projection_against_mongodb() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collection = database.collection::<Document>(SELECT_TEST_COLLECTION);
    collection
        .delete_many(doc! {})
        .await
        .expect("test collection should be reset");
    collection
        .insert_many(vec![
            doc! { "name": "Amina", "profile": { "city": "Harare" } },
            doc! { "name": "Tendai", "profile": { "city": "Bulawayo" } },
        ])
        .await
        .expect("fixture documents should be inserted");

    let plan = SelectPlan {
        collection: SELECT_TEST_COLLECTION.into(),
        projection: name_projection(),
        filter: Some(Predicate::Compare {
            path: FieldPath::top_level("profile").child("city"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("Harare".into()),
        }),
        limit: Some(10),
    };

    let outcome = execute_select(&database, &plan)
        .await
        .expect("SELECT plan should execute");
    assert_eq!(outcome.documents.len(), 1);
    assert_eq!(
        outcome.documents[0]
            .get_str("name")
            .expect("projection should include name"),
        "Amina"
    );
    assert!(!outcome.documents[0].contains_key("profile"));
}

/// Requires a running local `MongoDB` instance.
///
/// SQL `IS NULL` has deliberately stricter semantics than a bare `MongoDB`
/// `{ field: null }` filter: missing fields must not be treated as SQL nulls.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn keeps_null_missing_array_and_mixed_type_predicate_behavior_explicit() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collection = database.collection::<Document>(PREDICATE_MATRIX_COLLECTION);
    collection
        .delete_many(doc! {})
        .await
        .expect("test collection should be reset");
    collection
        .insert_many(vec![
            doc! { "name": "explicit-null", "status": null },
            doc! { "name": "missing-status" },
            doc! { "name": "string-status", "status": "active" },
            doc! { "name": "integer-status", "status": 1_i64 },
            doc! { "name": "array-status", "status": ["active", "pending"] },
            doc! { "name": "object-status", "status": { "value": "active" } },
        ])
        .await
        .expect("predicate matrix fixture should be inserted");

    let is_null = SelectPlan {
        collection: PREDICATE_MATRIX_COLLECTION.into(),
        projection: name_projection(),
        filter: Some(Predicate::IsNull {
            path: FieldPath::top_level("status"),
            negated: false,
        }),
        limit: None,
    };
    let null_outcome = execute_select(&database, &is_null)
        .await
        .expect("IS NULL plan should execute");
    assert_eq!(
        projected_names(&null_outcome.documents),
        BTreeSet::from(["explicit-null".to_owned()])
    );

    let is_not_null = SelectPlan {
        collection: PREDICATE_MATRIX_COLLECTION.into(),
        projection: name_projection(),
        filter: Some(Predicate::IsNull {
            path: FieldPath::top_level("status"),
            negated: true,
        }),
        limit: None,
    };
    let not_null_outcome = execute_select(&database, &is_not_null)
        .await
        .expect("IS NOT NULL plan should execute");
    assert_eq!(
        projected_names(&not_null_outcome.documents),
        BTreeSet::from([
            "array-status".to_owned(),
            "integer-status".to_owned(),
            "object-status".to_owned(),
            "string-status".to_owned(),
        ])
    );

    let string_equality = SelectPlan {
        collection: PREDICATE_MATRIX_COLLECTION.into(),
        projection: name_projection(),
        filter: Some(Predicate::Compare {
            path: FieldPath::top_level("status"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("active".into()),
        }),
        limit: None,
    };
    let string_outcome = execute_select(&database, &string_equality)
        .await
        .expect("mixed-type equality plan should execute");

    // MongoDB equality intentionally considers a scalar equal to an array
    // element. It does not coerce the integer or object fixture values.
    assert_eq!(
        projected_names(&string_outcome.documents),
        BTreeSet::from(["array-status".to_owned(), "string-status".to_owned()])
    );
}

/// Requires a running local `MongoDB` instance.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn persists_a_nested_insert_and_returns_the_actual_count() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collection = database.collection::<Document>(INSERT_TEST_COLLECTION);
    collection
        .delete_many(doc! {})
        .await
        .expect("test collection should be reset");

    let plan = InsertPlan {
        collection: INSERT_TEST_COLLECTION.into(),
        columns: vec![
            FieldPath::top_level("name"),
            FieldPath::top_level("profile").child("city"),
        ],
        rows: vec![vec![
            SqlValue::String("Amina".into()),
            SqlValue::String("Harare".into()),
        ]],
    };
    let outcome = execute_insert(&database, &plan)
        .await
        .expect("INSERT plan should execute");
    assert_eq!(outcome.inserted, 1);

    let stored = collection
        .find_one(doc! { "name": "Amina" })
        .await
        .expect("lookup should succeed")
        .expect("inserted document should exist");
    assert_eq!(
        stored
            .get_document("profile")
            .and_then(|profile| profile.get_str("city")),
        Ok("Harare")
    );
}

/// Requires a running local `MongoDB` instance.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn returns_a_structured_error_for_a_failed_insert() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collection = database.collection::<Document>(INSERT_FAILURE_COLLECTION);
    collection
        .delete_many(doc! {})
        .await
        .expect("test collection should be reset");
    collection
        .insert_one(doc! { "_id": "duplicate-id", "name": "existing" })
        .await
        .expect("fixture document should be inserted");

    let plan = InsertPlan {
        collection: INSERT_FAILURE_COLLECTION.into(),
        columns: vec![FieldPath::top_level("_id"), FieldPath::top_level("name")],
        rows: vec![vec![
            SqlValue::String("duplicate-id".into()),
            SqlValue::String("duplicate".into()),
        ]],
    };
    let error = execute_insert(&database, &plan)
        .await
        .expect_err("duplicate key should fail as a proxy error");
    assert_eq!(error.kind, ErrorKind::Database);
    assert!(error.message.contains("may have partially applied"));
}

/// Requires a running local `MongoDB` instance.
///
/// `MongoDB` ordered bulk inserts can persist earlier rows before a later
/// duplicate key failure. The proxy must not report this as an all-or-nothing
/// result or retry the statement.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn reports_a_partial_bulk_insert_without_retrying_or_hiding_persisted_rows() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collection = database.collection::<Document>(PARTIAL_INSERT_COLLECTION);
    collection
        .delete_many(doc! {})
        .await
        .expect("test collection should be reset");
    collection
        .insert_one(doc! { "_id": "duplicate-id", "name": "existing" })
        .await
        .expect("duplicate fixture should be inserted");

    let plan = InsertPlan {
        collection: PARTIAL_INSERT_COLLECTION.into(),
        columns: vec![FieldPath::top_level("_id"), FieldPath::top_level("name")],
        rows: vec![
            vec![
                SqlValue::String("before-duplicate".into()),
                SqlValue::String("persisted-before-failure".into()),
            ],
            vec![
                SqlValue::String("duplicate-id".into()),
                SqlValue::String("duplicate".into()),
            ],
            vec![
                SqlValue::String("after-duplicate".into()),
                SqlValue::String("must-not-be-inserted".into()),
            ],
        ],
    };
    let error = execute_insert(&database, &plan)
        .await
        .expect_err("ordered bulk insert should report its duplicate key failure");
    assert_eq!(error.kind, ErrorKind::Database);
    assert!(error.message.contains("may have partially applied"));

    assert!(
        collection
            .find_one(doc! { "_id": "before-duplicate" })
            .await
            .expect("partial-result lookup should succeed")
            .is_some(),
        "the successful row before the duplicate is intentionally visible"
    );
    assert!(
        collection
            .find_one(doc! { "_id": "after-duplicate" })
            .await
            .expect("post-failure lookup should succeed")
            .is_none(),
        "ordered inserts stop after the duplicate failure"
    );
}
