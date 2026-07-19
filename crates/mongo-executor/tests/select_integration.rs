//! Live coverage for deterministic `MongoDB` `find` execution.

use std::env;

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
        projection: Projection::Fields(vec![ProjectedField {
            path: FieldPath::top_level("name"),
            alias: None,
        }]),
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
