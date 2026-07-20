//! Bounded HTTP transport for the write-ambiguity resolver.
//!
//! Contract types and all decision validation live in `mongo-pg-ambiguity-policy`.
//! This crate has no `MongoDB` dependency and only sends a typed request before
//! returning a typed, non-executable response to the policy layer.

use std::time::Duration;

use mongo_pg_common::{ErrorKind, ProxyError};
use reqwest::{Client, Url};

pub use mongo_pg_ambiguity_policy::{
    RESOLUTION_CONTRACT_VERSION, ResolutionCandidate, ResolverRequest, ResolverResponse,
};

/// HTTP client configuration, including the explicit fail-closed timeout.
#[derive(Debug, Clone)]
pub struct ResolverClientConfig {
    /// Absolute `POST` endpoint, for example `http://localhost:8000/v1/resolve`.
    pub endpoint: String,
    /// Maximum time for the entire HTTP request.
    pub timeout: Duration,
}

impl ResolverClientConfig {
    /// Constructs a bounded resolver client configuration.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, timeout: Duration) -> Self {
        Self {
            endpoint: endpoint.into(),
            timeout,
        }
    }

    fn endpoint_url(&self) -> Result<Url, ProxyError> {
        if self.timeout.is_zero() {
            return Err(ProxyError::new(
                ErrorKind::InvalidInput,
                "resolver timeout must be greater than zero",
            ));
        }
        let endpoint = Url::parse(&self.endpoint).map_err(|_| {
            ProxyError::new(
                ErrorKind::InvalidInput,
                "resolver endpoint must be an absolute URL",
            )
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(ProxyError::new(
                ErrorKind::InvalidInput,
                "resolver endpoint must use HTTP or HTTPS",
            ));
        }
        Ok(endpoint)
    }
}

/// A client that transports resolver requests but cannot execute their output.
#[derive(Clone)]
pub struct ResolverClient {
    endpoint: Url,
    client: Client,
}

impl ResolverClient {
    /// Builds a client with the supplied whole-request timeout.
    ///
    /// # Errors
    ///
    /// Returns invalid-input errors for invalid configuration and dependency
    /// errors when the underlying HTTP client cannot be initialized.
    pub fn new(config: &ResolverClientConfig) -> Result<Self, ProxyError> {
        let endpoint = config.endpoint_url()?;
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|_| {
                ProxyError::new(
                    ErrorKind::Dependency,
                    "failed to initialize resolver HTTP client",
                )
            })?;
        Ok(Self { endpoint, client })
    }

    /// Sends one versioned request and decodes a non-executable response.
    ///
    /// This method deliberately does not validate confidence, target-path
    /// correlation, or allowed decisions. The policy layer validates all of
    /// those values against its original ambiguity evidence before execution.
    ///
    /// # Errors
    ///
    /// Returns a dependency error for transport, timeout, non-success, or
    /// malformed JSON failures.
    pub async fn resolve(&self, request: &ResolverRequest) -> Result<ResolverResponse, ProxyError> {
        self.client
            .post(self.endpoint.clone())
            .json(request)
            .send()
            .await
            .map_err(|_| {
                ProxyError::new(
                    ErrorKind::Dependency,
                    "ambiguity resolver is unavailable or timed out",
                )
            })?
            .error_for_status()
            .map_err(|_| {
                ProxyError::new(
                    ErrorKind::Dependency,
                    "ambiguity resolver returned an HTTP error",
                )
            })?
            .json::<ResolverResponse>()
            .await
            .map_err(|_| {
                ProxyError::new(
                    ErrorKind::Dependency,
                    "ambiguity resolver returned an invalid response",
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, time::Duration};

    use mongo_pg_ambiguity_policy::{AmbiguityKind, ResolverAmbiguityEvidence, WriteOperation};

    use super::{
        RESOLUTION_CONTRACT_VERSION, ResolutionCandidate, ResolverClient, ResolverClientConfig,
        ResolverRequest,
    };

    fn request() -> ResolverRequest {
        ResolverRequest {
            contract_version: RESOLUTION_CONTRACT_VERSION.into(),
            schema_profile_version: 7,
            operation: WriteOperation::Update,
            target_path: vec!["profile".into(), "address".into(), "city".into()],
            ambiguity: ResolverAmbiguityEvidence {
                kinds: BTreeSet::from([AmbiguityKind::MissingFromSampledDocuments]),
                observed_types: vec!["string".into()],
                observed_shapes: vec!["scalar".into()],
                missing_documents: 2,
            },
            allowed_candidates: BTreeSet::from([
                ResolutionCandidate::UseNestedPath,
                ResolutionCandidate::Reject,
            ]),
        }
    }

    #[test]
    fn serializes_the_shared_non_executable_contract() {
        let json = serde_json::to_string(&request()).expect("request serializes");
        assert!(json.contains("\"v2\""));
        assert!(json.contains("\"use_nested_path\""));
        assert!(json.contains("\"allowed_candidates\""));
        assert!(!json.contains("pipeline"));
        assert!(!json.contains("operator"));
    }

    #[test]
    fn rejects_invalid_client_configuration() {
        let zero_timeout =
            ResolverClientConfig::new("http://localhost:8000/v1/resolve", Duration::ZERO);
        assert!(ResolverClient::new(&zero_timeout).is_err());

        let invalid_scheme =
            ResolverClientConfig::new("file:///tmp/resolver", Duration::from_secs(1));
        assert!(ResolverClient::new(&invalid_scheme).is_err());
    }
}
