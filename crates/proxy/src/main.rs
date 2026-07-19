//! `PostgreSQL` wire-protocol proxy entry point.

use std::process::ExitCode;

use mongo_pg_proxy::{ProxyConfig, run_server};

#[tokio::main]
async fn main() -> ExitCode {
    match ProxyConfig::from_environment() {
        Ok(config) => match run_server(config).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("proxy failed: {}", error.message);
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("proxy configuration failed: {}", error.message);
            ExitCode::FAILURE
        }
    }
}
