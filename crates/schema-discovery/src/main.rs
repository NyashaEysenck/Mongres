//! Schema-discovery command-line entry point.

use std::env;
use std::process::ExitCode;

use mongo_pg_schema_discovery::discover_and_persist_collection;
use mongodb::Client;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("schema discovery failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let uri = required_environment("MONGO_URI")?;
    let database = required_environment("MONGO_DATABASE")?;
    let collection = required_environment("MONGO_COLLECTION")?;
    let sample_size = optional_environment("MONGO_SAMPLE_SIZE", 100)?;
    let client = Client::with_uri_str(uri).await?;
    let profile =
        discover_and_persist_collection(&client, &database, &collection, sample_size).await?;

    println!(
        "discovered {} fields from {} sampled documents in {}.{}",
        profile.fields.len(),
        profile.sampled_documents,
        database,
        collection
    );
    Ok(())
}

fn required_environment(name: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    env::var(name).map_err(|_| format!("required environment variable is missing: {name}").into())
}

fn optional_environment(
    name: &str,
    default: usize,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| format!("{name} must be a positive integer").into()),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}
