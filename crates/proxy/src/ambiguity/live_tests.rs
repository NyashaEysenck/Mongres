//! Live proxy-boundary coverage for `MongoDB` writes that use the resolver.

use std::{collections::BTreeSet, env, sync::Mutex, time::Duration};

use mongo_pg_mongo_executor::{deterministic_write_client, execute_write};
use mongo_pg_resolver_client::{ResolverClient, ResolverClientConfig};
use mongo_pg_schema_discovery::{
    FieldPath, FieldProfile, ObservedShape, ObservedType, SchemaProfile,
};
use mongo_pg_sql_engine::{
    AssignmentPlan, ComparisonOperator, Predicate, SqlValue, StatementPlan, UpdatePlan,
};
use mongodb::bson::{Document, doc};

use super::{record_execution, resolve_write_plan};

const TEST_COLLECTION: &str = "proxy_ambiguity_integration";

/// Requires a running `MongoDB` instance and a running ambiguity resolver.
///
/// Run with:
/// `MONGO_INTEGRATION_URI=mongodb://localhost:27017 \
/// MONGO_INTEGRATION_DATABASE=mongo_pg_proxy_test \
/// AMBIGUITY_RESOLVER_URL=http://127.0.0.1:8000/v1/resolve \
/// cargo test -p mongo-pg-proxy live_mongodb_and_resolver_write_flow -- --ignored`
#[tokio::test]
#[ignore = "requires running MongoDB and ambiguity resolver services"]
async fn live_mongodb_and_resolver_write_flow() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let resolver_url = env::var("AMBIGUITY_RESOLVER_URL")
        .expect("AMBIGUITY_RESOLVER_URL must point to a running resolver");
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
        .insert_many(vec![
            doc! { "name": "Amina", "status": "new" },
            doc! {
                "name": "Tendai",
                "status": "new",
                "profile": { "city": "Bulawayo" },
            },
        ])
        .await
        .expect("fixture documents should be inserted");

    let schema = schema();
    let audit_records = Mutex::new(Vec::new());

    let clear_outcome = resolve_and_execute(
        &database,
        update(FieldPath::top_level("status"), "active"),
        &schema,
        resolver("http://127.0.0.1:1/v1/resolve"),
        0.8,
        &audit_records,
    )
    .await
    .expect("clear write should bypass the unavailable resolver");
    assert_eq!(clear_outcome.matched, 1);
    assert_eq!(clear_outcome.modified, 1);
    assert!(
        audit_records
            .lock()
            .expect("audit records should be readable")
            .is_empty()
    );
    assert_eq!(
        stored_string(&collection, "Amina", "status").await,
        "active"
    );

    let nested_path = FieldPath::top_level("profile").child("city");
    let nested_outcome = resolve_and_execute(
        &database,
        update(nested_path.clone(), "Harare"),
        &schema,
        resolver(resolver_url),
        0.8,
        &audit_records,
    )
    .await
    .expect("resolver-approved nested write should execute");
    assert_eq!(nested_outcome.matched, 1);
    assert_eq!(nested_outcome.modified, 1);
    assert_eq!(
        stored_document(&collection, "Amina")
            .await
            .get_document("profile")
            .expect("profile should be created as a nested document")
            .get_str("city"),
        Ok("Harare")
    );

    let failed = resolve_and_execute(
        &database,
        update(nested_path, "Mutare"),
        &schema,
        resolver("http://127.0.0.1:1/v1/resolve"),
        0.8,
        &audit_records,
    )
    .await;
    assert!(failed.is_err());
    assert_eq!(
        stored_document(&collection, "Amina")
            .await
            .get_document("profile")
            .expect("profile should remain after failed resolution")
            .get_str("city"),
        Ok("Harare")
    );
    assert_eq!(
        audit_records
            .lock()
            .expect("audit records should be readable")
            .len(),
        2
    );
}

async fn resolve_and_execute(
    database: &mongodb::Database,
    plan: StatementPlan,
    schema: &SchemaProfile,
    resolver: ResolverClient,
    minimum_confidence: f64,
    audit_records: &Mutex<Vec<mongo_pg_ambiguity_policy::audit::AmbiguityAuditRecord>>,
) -> Result<mongo_pg_mongo_executor::WriteOutcome, mongo_pg_common::ProxyError> {
    let write_plan =
        resolve_write_plan(plan, schema, &resolver, minimum_confidence, audit_records).await?;
    let outcome = execute_write(database, &write_plan.plan).await?;
    record_execution(
        schema,
        write_plan.audit_context.as_ref(),
        outcome,
        audit_records,
    );
    Ok(outcome)
}

fn schema() -> SchemaProfile {
    SchemaProfile {
        profile_version: 7,
        sampled_documents: 2,
        fields: vec![
            field(FieldPath::top_level("name"), 2, 0),
            field(FieldPath::top_level("status"), 2, 0),
            field(FieldPath::top_level("profile").child("city"), 1, 1),
        ],
    }
}

fn field(path: FieldPath, present_documents: usize, missing_documents: usize) -> FieldProfile {
    FieldProfile {
        path,
        present_documents,
        missing_documents,
        observed_types: BTreeSet::from([ObservedType::String]),
        observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
        has_dotted_key_collision: false,
    }
}

fn update(path: FieldPath, value: &str) -> StatementPlan {
    StatementPlan::Update(UpdatePlan {
        collection: TEST_COLLECTION.to_owned(),
        assignments: vec![AssignmentPlan {
            path,
            value: SqlValue::String(value.to_owned()),
        }],
        filter: Predicate::Compare {
            path: FieldPath::top_level("name"),
            operator: ComparisonOperator::Equal,
            value: SqlValue::String("Amina".to_owned()),
        },
    })
}

fn resolver(endpoint: impl Into<String>) -> ResolverClient {
    let config = ResolverClientConfig::new(endpoint, Duration::from_secs(45));
    ResolverClient::new(&config).expect("test resolver endpoint should be valid")
}

async fn stored_string(
    collection: &mongodb::Collection<Document>,
    name: &str,
    field: &str,
) -> String {
    stored_document(collection, name)
        .await
        .get_str(field)
        .expect("stored field should be a string")
        .to_owned()
}

async fn stored_document(collection: &mongodb::Collection<Document>, name: &str) -> Document {
    collection
        .find_one(doc! { "name": name })
        .await
        .expect("lookup should succeed")
        .expect("fixture document should exist")
}
