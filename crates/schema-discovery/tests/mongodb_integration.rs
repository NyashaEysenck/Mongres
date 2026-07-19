//! Integration coverage for real MongoDB sampling and profile persistence.

use std::env;

use mongo_pg_schema_discovery::{METADATA_COLLECTION, discover_and_persist_collection};
use mongodb::{
    Client,
    bson::{Document, doc},
};

const TEST_COLLECTION: &str = "schema_discovery_integration";

/// Requires a MongoDB instance, normally started with `docker compose up -d`.
///
/// Run with:
/// `MONGO_INTEGRATION_URI=mongodb://localhost:27017 \
/// MONGO_INTEGRATION_DATABASE=mongo_pg_proxy_test \
/// cargo test -p mongo-pg-schema-discovery --test mongodb_integration -- --ignored`
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn samples_a_real_collection_and_upserts_its_profile() {
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
    database
        .collection::<Document>(METADATA_COLLECTION)
        .delete_many(doc! { "database": &database_name, "collection": TEST_COLLECTION })
        .await
        .expect("test metadata should be reset");

    collection
        .insert_many(vec![
            doc! {
                "name": "Amina",
                "profile": { "address": { "city": "Harare" } },
            },
            doc! {
                "name": "Tendai",
                "profile": { "address": { "city": "Bulawayo" } },
            },
            doc! { "name": "Nyasha" },
        ])
        .await
        .expect("fixture documents should be inserted");

    let profile = discover_and_persist_collection(&client, &database_name, TEST_COLLECTION, 10)
        .await
        .expect("profile discovery should succeed");
    assert_eq!(profile.sampled_documents, 3);
    assert!(
        profile
            .fields
            .iter()
            .any(|field| field.path.display_name() == "profile.address.city")
    );

    let stored = database
        .collection::<Document>(METADATA_COLLECTION)
        .find_one(doc! { "database": &database_name, "collection": TEST_COLLECTION })
        .await
        .expect("metadata lookup should succeed")
        .expect("schema profile should be persisted");
    assert!(stored.contains_key("profile"));
}
