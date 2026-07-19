//! Live coverage for deterministic `MongoDB` `find` execution.

use std::env;

use mongo_pg_mongo_executor::execute_select;
use mongo_pg_schema_discovery::FieldPath;
use mongo_pg_sql_engine::{
    ComparisonOperator, Predicate, ProjectedField, Projection, SelectPlan, SqlValue,
};
use mongodb::{
    Client,
    bson::{Document, doc},
};

const TEST_COLLECTION: &str = "executor_select_integration";

/// Requires a running local `MongoDB` instance.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn executes_a_nested_filter_and_projection_against_mongodb() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = Client::with_uri_str(uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collection = database.collection::<Document>(TEST_COLLECTION);
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
        collection: TEST_COLLECTION.into(),
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
