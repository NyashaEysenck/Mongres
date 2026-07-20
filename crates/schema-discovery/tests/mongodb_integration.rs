//! Integration coverage for real `MongoDB` sampling and profile persistence.

use std::env;

use mongo_pg_schema_discovery::{
    METADATA_COLLECTION, discover_and_persist_collection, discover_and_persist_collections,
    load_required_persisted_profiles,
};
use mongodb::{
    Client,
    bson::{Document, doc},
};

const TEST_COLLECTION: &str = "schema_discovery_integration";
const SECOND_TEST_COLLECTION: &str = "schema_discovery_integration_orders";

/// Requires a `MongoDB` instance, normally started with `docker compose up -d`.
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

/// Ensures per-collection profiles persist separately and can be loaded as one
/// configured allowlist without schema fields crossing collection boundaries.
#[tokio::test]
#[ignore = "requires a running MongoDB instance"]
async fn persists_and_loads_multiple_collection_profiles_in_isolation() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let client = Client::with_uri_str(uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = client.database(&database_name);
    let collections = vec![
        TEST_COLLECTION.to_owned(),
        SECOND_TEST_COLLECTION.to_owned(),
    ];

    for collection_name in &collections {
        database
            .collection::<Document>(collection_name)
            .delete_many(doc! {})
            .await
            .expect("test collection should be reset");
        database
            .collection::<Document>(METADATA_COLLECTION)
            .delete_many(doc! { "database": &database_name, "collection": collection_name })
            .await
            .expect("test metadata should be reset");
    }

    database
        .collection::<Document>(TEST_COLLECTION)
        .insert_one(doc! { "name": "Amina", "profile": { "city": "Harare" } })
        .await
        .expect("customer fixture should be inserted");
    database
        .collection::<Document>(SECOND_TEST_COLLECTION)
        .insert_one(doc! { "order_total": 42, "reference": "ORD-1" })
        .await
        .expect("order fixture should be inserted");

    let discovered = discover_and_persist_collections(&client, &database_name, &collections, 10)
        .await
        .expect("batch profile discovery should succeed");
    assert_eq!(discovered.len(), 2);
    assert!(
        discovered[TEST_COLLECTION]
            .fields
            .iter()
            .any(|field| field.path.display_name() == "profile.city")
    );
    assert!(
        discovered[SECOND_TEST_COLLECTION]
            .fields
            .iter()
            .any(|field| field.path.display_name() == "order_total")
    );
    assert!(
        !discovered[TEST_COLLECTION]
            .fields
            .iter()
            .any(|field| field.path.display_name() == "order_total")
    );

    let loaded = load_required_persisted_profiles(&database, &collections)
        .await
        .expect("batch profile load should succeed");
    assert_eq!(loaded, discovered);
}
