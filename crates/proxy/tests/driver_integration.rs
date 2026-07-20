//! Live PostgreSQL-driver coverage for the proxy wire boundary.

use std::{env, net::TcpListener as StdTcpListener, time::Duration};

use mongo_pg_mongo_executor::deterministic_write_client;
use mongo_pg_proxy::{ProxyAuthConfig, ProxyConfig, run_server};
use mongo_pg_schema_discovery::discover_and_persist_collection;
use mongodb::bson::{Document, doc};
use tokio::{task::JoinHandle, time::sleep};
use tokio_postgres::{Client as PgClient, NoTls};

const TEST_COLLECTION: &str = "proxy_driver_integration";

/// Requires a running local `MongoDB` instance.
///
/// Run with:
/// `MONGO_INTEGRATION_URI=mongodb://localhost:27017 \
/// MONGO_INTEGRATION_DATABASE=mongo_pg_proxy_test \
/// cargo test -p mongo-pg-proxy --test driver_integration -- --ignored`
#[tokio::test]
#[ignore = "requires a running MongoDB instance and local TCP listener access"]
async fn tokio_postgres_executes_bound_select_and_update() {
    let uri = env::var("MONGO_INTEGRATION_URI")
        .expect("MONGO_INTEGRATION_URI must be set for the MongoDB integration test");
    let database_name = env::var("MONGO_INTEGRATION_DATABASE")
        .expect("MONGO_INTEGRATION_DATABASE must be set for the MongoDB integration test");
    let mongo_client = deterministic_write_client(&uri)
        .await
        .expect("integration MongoDB client should connect");
    let database = mongo_client.database(&database_name);
    let collection = database.collection::<Document>(TEST_COLLECTION);

    collection
        .delete_many(doc! {})
        .await
        .expect("test collection should be reset");
    collection
        .insert_many(vec![
            doc! {
                "name": "Amina",
                "age": 42_i64,
                "active": true,
                "profile": { "city": "Harare" },
            },
            doc! {
                "name": "Tendai",
                "age": 37_i64,
                "active": false,
                "profile": { "city": "Bulawayo" },
            },
        ])
        .await
        .expect("fixture documents should be inserted");

    discover_and_persist_collection(&mongo_client, &database_name, TEST_COLLECTION, 10)
        .await
        .expect("schema profile should be persisted before proxy startup");

    let port = free_local_port();
    let proxy = spawn_proxy(uri, database_name.clone(), port);
    let pg_client = connect_driver(port, &database_name).await;

    let selected = pg_client
        .query_one(
            &format!("SELECT name FROM {TEST_COLLECTION} WHERE age = $1 AND active = $2"),
            &[&42_i64, &true],
        )
        .await
        .expect("bound SELECT should execute through the PostgreSQL driver");
    assert_eq!(selected.get::<_, String>(0), "Amina");

    let updated = pg_client
        .execute(
            &format!("UPDATE {TEST_COLLECTION} SET profile.city = $1 WHERE name = $2"),
            &[&"Mutare", &"Amina"],
        )
        .await
        .expect("bound UPDATE should execute through the PostgreSQL driver");
    assert_eq!(updated, 1);

    let stored = collection
        .find_one(doc! { "name": "Amina" })
        .await
        .expect("lookup should succeed")
        .expect("fixture document should remain present");
    assert_eq!(
        stored
            .get_document("profile")
            .and_then(|profile| profile.get_str("city")),
        Ok("Mutare")
    );

    proxy.abort();
}

fn spawn_proxy(uri: String, database_name: String, port: u16) -> JoinHandle<()> {
    tokio::spawn(async move {
        let config = ProxyConfig {
            mongo_uri: uri,
            database_name,
            collection_names: vec![TEST_COLLECTION.to_owned()],
            listen_address: format!("127.0.0.1:{port}"),
            resolver_url: "http://127.0.0.1:1/v1/resolve".to_owned(),
            resolver_timeout: Duration::from_millis(250),
            resolver_minimum_confidence: 0.8,
            auth: ProxyAuthConfig::Trust,
        };
        run_server(config)
            .await
            .expect("proxy server should run for the duration of the test");
    })
}

async fn connect_driver(port: u16, database_name: &str) -> PgClient {
    let connection_string =
        format!("host=127.0.0.1 port={port} user=integration dbname={database_name}");

    for _ in 0..40 {
        match tokio_postgres::connect(&connection_string, NoTls).await {
            Ok((client, connection)) => {
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        eprintln!("PostgreSQL driver connection ended with an error: {error}");
                    }
                });
                return client;
            }
            Err(_) => sleep(Duration::from_millis(50)).await,
        }
    }

    panic!("proxy did not accept a PostgreSQL driver connection on port {port}");
}

fn free_local_port() -> u16 {
    StdTcpListener::bind("127.0.0.1:0")
        .expect("test should reserve an ephemeral local port")
        .local_addr()
        .expect("test listener should have a local address")
        .port()
}
