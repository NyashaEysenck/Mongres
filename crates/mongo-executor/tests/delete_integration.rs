//! Live coverage for deterministic `MongoDB` `delete_many` execution.

use std::env;

use mongo_pg_mongo_executor::{deterministic_write_client, execute_delete};
use mongo_pg_schema_discovery::FieldPath;
use mongo_pg_sql_engine::{ComparisonOperator, DeletePlan, Predicate, SqlValue};
use mongodb::bson::{Document, doc};

const MATCHING_DELETE_COLLECTION: &str = "executor_delete_matching_integration";
const NO_MATCH_DELETE_COLLECTION: &str = "executor_delete_no_match_integration";

async fn test_database() -> mongodb::Database {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    client.database(&database_name)
}

async fn reset_fixture(database: &mongodb::Database, collection_name: &str) {
    let collection = database.collection::<Document>(collection_name);
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
}

/// Requires a running local `MongoDB` instance.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn deletes_matching_documents_and_returns_the_actual_count() {
    let database = test_database().await;
    reset_fixture(&database, MATCHING_DELETE_COLLECTION).await;
    let collection = database.collection::<Document>(MATCHING_DELETE_COLLECTION);
    let plan = DeletePlan {
        collection: MATCHING_DELETE_COLLECTION.into(),
        filter: Predicate::Compare {
            path: FieldPath::top_level("profile").child("city"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("Harare".into()),
        },
    };

    let outcome = execute_delete(&database, &plan)
        .await
        .expect("DELETE plan should execute");
    assert_eq!(outcome.deleted, 1);

    assert!(
        collection
            .find_one(doc! { "name": "Amina" })
            .await
            .expect("deleted document lookup should succeed")
            .is_none()
    );
    assert!(
        collection
            .find_one(doc! { "name": "Tendai", "profile.city": "Bulawayo" })
            .await
            .expect("remaining document lookup should succeed")
            .is_some()
    );
}

/// Requires a running local `MongoDB` instance.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn reports_zero_when_no_documents_match_a_delete() {
    let database = test_database().await;
    reset_fixture(&database, NO_MATCH_DELETE_COLLECTION).await;
    let collection = database.collection::<Document>(NO_MATCH_DELETE_COLLECTION);
    let plan = DeletePlan {
        collection: NO_MATCH_DELETE_COLLECTION.into(),
        filter: Predicate::Compare {
            path: FieldPath::top_level("name"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("Missing".into()),
        },
    };

    let outcome = execute_delete(&database, &plan)
        .await
        .expect("DELETE plan should execute");
    assert_eq!(outcome.deleted, 0);
    assert_eq!(
        collection
            .count_documents(doc! {})
            .await
            .expect("fixture document count should succeed"),
        2
    );
}
