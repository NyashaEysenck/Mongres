//! Schema-discovery command-line entry point.

use std::env;
use std::process::ExitCode;

use mongo_pg_schema_discovery::{discover_and_persist_collections, validated_collection_names};
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
    let collections = configured_collections()?;
    let sample_size = optional_environment("MONGO_SAMPLE_SIZE", 100)?;
    let client = Client::with_uri_str(uri).await?;
    let profiles =
        discover_and_persist_collections(&client, &database, &collections, sample_size).await?;

    for (collection, profile) in profiles {
        println!(
            "discovered {} fields from {} sampled documents in {}.{}",
            profile.fields.len(),
            profile.sampled_documents,
            database,
            collection
        );
    }
    Ok(())
}

fn configured_collections() -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let list = env::var("MONGO_COLLECTIONS").ok();
    let legacy = match env::var("MONGO_COLLECTION") {
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => None,
        Err(error) => return Err(error.into()),
    };

    parse_configured_collections(list.as_deref(), legacy.as_deref())
}

fn parse_configured_collections(
    configured_list: Option<&str>,
    legacy_collection: Option<&str>,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let configured = match configured_list {
        Some(value) => value
            .split(',')
            .map(str::trim)
            .map(ToOwned::to_owned)
            .collect(),
        None => vec![legacy_collection
            .ok_or("required environment variable is missing: MONGO_COLLECTION or MONGO_COLLECTIONS")?
            .to_owned()],
    };

    validated_collection_names(&configured)
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

#[cfg(test)]
mod tests {
    use super::parse_configured_collections;

    #[test]
    fn collection_list_takes_precedence_over_legacy_single_collection() {
        let result = parse_configured_collections(Some("orders,customers"), Some("legacy"))
            .expect("list should parse");
        assert_eq!(result, vec!["customers", "orders"]);
    }

    #[test]
    fn legacy_single_collection_is_supported_for_existing_deployments() {
        let result = parse_configured_collections(None, Some("customers"))
            .expect("legacy configuration should parse");
        assert_eq!(result, vec!["customers"]);
    }
}
