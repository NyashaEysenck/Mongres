//! Live coverage for deterministic nested `MongoDB` update execution.

use std::env;

use mongo_pg_common::ErrorKind;
use mongo_pg_mongo_executor::{deterministic_write_client, execute_update};
use mongo_pg_schema_discovery::FieldPath;
use mongo_pg_sql_engine::{AssignmentPlan, ComparisonOperator, Predicate, SqlValue, UpdatePlan};
use mongodb::bson::{Document, doc};

const TEST_COLLECTION: &str = "executor_update_integration";

/// Requires a running local `MongoDB` instance.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn updates_a_nested_path_and_reports_actual_counts() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collection = database.collection::<Document>(TEST_COLLECTION);
    collection
        .delete_many(doc! {})
        .await
        .expect("test collection should be reset");
    collection
        .insert_one(doc! {
            "name": "Amina",
            "profile": { "city": "Harare", "country": "Zimbabwe" },
        })
        .await
        .expect("fixture document should be inserted");

    let city_path = FieldPath::top_level("profile").child("city");
    let matching_filter = Predicate::Compare {
        path: FieldPath::top_level("name"),
        operator: ComparisonOperator::Equal,
        value: SqlValue::String("Amina".into()),
    };
    let update = UpdatePlan {
        collection: TEST_COLLECTION.into(),
        assignments: vec![AssignmentPlan {
            path: city_path,
            value: SqlValue::String("Bulawayo".into()),
        }],
        filter: matching_filter,
    };

    let outcome = execute_update(&database, &update)
        .await
        .expect("nested UPDATE plan should execute");
    assert_eq!(outcome.matched, 1);
    assert_eq!(outcome.modified, 1);
    assert_eq!(outcome.inserted, 0);
    assert_eq!(outcome.deleted, 0);

    let stored = collection
        .find_one(doc! { "name": "Amina" })
        .await
        .expect("lookup should succeed")
        .expect("fixture document should still exist");
    let profile = stored
        .get_document("profile")
        .expect("nested profile document should remain a document");
    assert_eq!(profile.get_str("city"), Ok("Bulawayo"));
    assert_eq!(profile.get_str("country"), Ok("Zimbabwe"));

    let no_match = UpdatePlan {
        collection: TEST_COLLECTION.into(),
        assignments: vec![AssignmentPlan {
            path: FieldPath::top_level("profile").child("city"),
            value: SqlValue::String("Mutare".into()),
        }],
        filter: Predicate::Compare {
            path: FieldPath::top_level("name"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("Missing customer".into()),
        },
    };
    let no_match_outcome = execute_update(&database, &no_match)
        .await
        .expect("non-matching UPDATE plan should execute");
    assert_eq!(no_match_outcome.matched, 0);
    assert_eq!(no_match_outcome.modified, 0);
}

/// Requires a running local `MongoDB` instance.
///
/// A SQL-visible dotted name can denote either a nested path or a literal
/// `MongoDB` key. Until a dedicated deterministic literal-key primitive exists,
/// the executor rejects the literal interpretation before issuing a write.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn rejects_literal_dotted_key_updates_without_mutating_nested_documents() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collection = database.collection::<Document>(TEST_COLLECTION);
    collection
        .delete_many(doc! {})
        .await
        .expect("test collection should be reset");
    collection
        .insert_one(doc! {
            "name": "Amina",
            "profile": { "city": "Harare" },
        })
        .await
        .expect("fixture document should be inserted");

    let update = UpdatePlan {
        collection: TEST_COLLECTION.into(),
        assignments: vec![AssignmentPlan {
            path: FieldPath::top_level("profile.city"),
            value: SqlValue::String("literal-key-value".into()),
        }],
        filter: Predicate::Compare {
            path: FieldPath::top_level("name"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("Amina".into()),
        },
    };
    let error = execute_update(&database, &update)
        .await
        .expect_err("literal dotted-key update must fail closed");
    assert_eq!(error.kind, ErrorKind::FeatureNotSupported);

    let stored = collection
        .find_one(doc! { "name": "Amina" })
        .await
        .expect("fixture lookup should succeed")
        .expect("fixture document should remain present");
    assert_eq!(
        stored
            .get_document("profile")
            .and_then(|profile| profile.get_str("city")),
        Ok("Harare")
    );
    assert!(!stored.contains_key("profile.city"));
}
