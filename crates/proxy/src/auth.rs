//! Configurable `PostgreSQL` startup authentication for the proxy boundary.

use std::{env, fmt::Debug};

use async_trait::async_trait;
use futures_util::{Sink, SinkExt};
use mongo_pg_common::{ErrorKind, ProxyError};
use pgwire::{
    api::{
        ClientInfo, METADATA_USER, PgWireConnectionState,
        auth::{
            DefaultServerParameterProvider, StartupHandler, finish_authentication,
            save_startup_parameters_to_metadata,
        },
    },
    error::{ErrorInfo, PgWireError, PgWireResult},
    messages::{
        PgWireBackendMessage, PgWireFrontendMessage, response::ErrorResponse,
        startup::Authentication,
    },
};

/// `PostgreSQL` startup settings that never expose the configured password in debug output.
#[derive(Clone, PartialEq, Eq)]
pub enum ProxyAuthConfig {
    /// Accept any `PostgreSQL` startup request without password verification.
    Trust,
    /// Require one configured username and cleartext password.
    CleartextPassword { username: String, password: String },
}

impl Debug for ProxyAuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trust => formatter.write_str("ProxyAuthConfig::Trust"),
            Self::CleartextPassword { username, .. } => formatter
                .debug_struct("ProxyAuthConfig::CleartextPassword")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
        }
    }
}

impl ProxyAuthConfig {
    /// Reads `PROXY_AUTH_*` settings without returning a configured password in errors.
    ///
    /// # Errors
    ///
    /// Returns an error when `PROXY_AUTH_MODE` is unsupported, or when cleartext
    /// authentication is selected without both required credentials.
    pub fn from_environment() -> Result<Self, ProxyError> {
        let mode = env::var("PROXY_AUTH_MODE").unwrap_or_else(|_| "trust".to_owned());
        Self::parse(
            &mode,
            env::var("PROXY_AUTH_USER").ok(),
            env::var("PROXY_AUTH_PASSWORD").ok(),
        )
    }

    fn parse(
        mode: &str,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<Self, ProxyError> {
        match mode.to_ascii_lowercase().as_str() {
            "trust" => Ok(Self::Trust),
            "cleartext" => {
                let username = required_setting("PROXY_AUTH_USER", username)?;
                let password = required_setting("PROXY_AUTH_PASSWORD", password)?;
                Ok(Self::CleartextPassword { username, password })
            }
            _ => Err(ProxyError::new(
                ErrorKind::InvalidInput,
                "PROXY_AUTH_MODE must be 'trust' or 'cleartext'",
            )),
        }
    }
}

/// pgwire startup handler that applies the configured authentication mode.
#[derive(Debug, Clone)]
pub struct ProxyStartupHandler {
    config: ProxyAuthConfig,
}

impl ProxyStartupHandler {
    /// Creates a startup handler from validated proxy authentication settings.
    #[must_use]
    pub fn new(config: ProxyAuthConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl StartupHandler for ProxyStartupHandler {
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        match &self.config {
            ProxyAuthConfig::Trust => trust_startup(client, message).await,
            ProxyAuthConfig::CleartextPassword { username, password } => {
                cleartext_startup(client, message, username, password).await
            }
        }
    }
}

async fn trust_startup<C>(client: &mut C, message: PgWireFrontendMessage) -> PgWireResult<()>
where
    C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    if let PgWireFrontendMessage::Startup(startup) = message {
        save_startup_parameters_to_metadata(client, &startup);
        finish_authentication(client, &DefaultServerParameterProvider::default()).await;
    }
    Ok(())
}

async fn cleartext_startup<C>(
    client: &mut C,
    message: PgWireFrontendMessage,
    expected_username: &str,
    expected_password: &str,
) -> PgWireResult<()>
where
    C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    match message {
        PgWireFrontendMessage::Startup(startup) => {
            save_startup_parameters_to_metadata(client, &startup);
            client.set_state(PgWireConnectionState::AuthenticationInProgress);
            client
                .send(PgWireBackendMessage::Authentication(
                    Authentication::CleartextPassword,
                ))
                .await?;
        }
        PgWireFrontendMessage::PasswordMessageFamily(password_message) => {
            let received_password = password_message.into_password()?;
            let username_matches = client
                .metadata()
                .get(METADATA_USER)
                .is_some_and(|username| username == expected_username);
            let password_matches =
                received_password.password.as_bytes() == expected_password.as_bytes();
            if username_matches && password_matches {
                finish_authentication(client, &DefaultServerParameterProvider::default()).await;
            } else {
                let error = ErrorResponse::from(ErrorInfo::new(
                    "FATAL".to_owned(),
                    "28P01".to_owned(),
                    "password authentication failed".to_owned(),
                ));
                client
                    .feed(PgWireBackendMessage::ErrorResponse(error))
                    .await?;
                client.close().await?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn required_setting(name: &str, value: Option<String>) -> Result<String, ProxyError> {
    value.filter(|value| !value.is_empty()).ok_or_else(|| {
        ProxyError::new(
            ErrorKind::InvalidInput,
            format!("{name} is required when PROXY_AUTH_MODE=cleartext"),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::{fmt::Debug, sync::Arc};

    use async_trait::async_trait;
    use futures_util::Sink;
    use pgwire::{
        api::{
            ClientInfo, PgWireHandlerFactory,
            copy::NoopCopyHandler,
            query::{PlaceholderExtendedQueryHandler, SimpleQueryHandler},
            results::{Response, Tag},
        },
        error::{PgWireError, PgWireResult},
        messages::PgWireBackendMessage,
        tokio::process_socket,
    };
    use tokio::net::{TcpListener, TcpStream};
    use tokio_postgres::{NoTls, error::SqlState};

    use super::{ProxyAuthConfig, ProxyStartupHandler};

    struct AuthTestQueryHandler;

    #[async_trait]
    impl SimpleQueryHandler for AuthTestQueryHandler {
        async fn do_query<'a, C>(
            &self,
            _client: &mut C,
            _query: &'a str,
        ) -> PgWireResult<Vec<Response<'a>>>
        where
            C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
            C::Error: Debug,
            PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
        {
            Ok(vec![Response::Execution(Tag::new("OK").with_rows(1))])
        }
    }

    struct AuthTestFactory {
        auth: ProxyAuthConfig,
        query_handler: Arc<AuthTestQueryHandler>,
    }

    impl PgWireHandlerFactory for AuthTestFactory {
        type StartupHandler = ProxyStartupHandler;
        type SimpleQueryHandler = AuthTestQueryHandler;
        type ExtendedQueryHandler = PlaceholderExtendedQueryHandler;
        type CopyHandler = NoopCopyHandler;

        fn simple_query_handler(&self) -> Arc<Self::SimpleQueryHandler> {
            self.query_handler.clone()
        }

        fn extended_query_handler(&self) -> Arc<Self::ExtendedQueryHandler> {
            Arc::new(PlaceholderExtendedQueryHandler)
        }

        fn startup_handler(&self) -> Arc<Self::StartupHandler> {
            Arc::new(ProxyStartupHandler::new(self.auth.clone()))
        }

        fn copy_handler(&self) -> Arc<Self::CopyHandler> {
            Arc::new(NoopCopyHandler)
        }
    }

    async fn start_auth_server(auth: ProxyAuthConfig) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test PostgreSQL listener");
        let address = listener.local_addr().expect("read test listener address");
        let factory = Arc::new(AuthTestFactory {
            auth,
            query_handler: Arc::new(AuthTestQueryHandler),
        });
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept test client");
            serve_one_connection(socket, factory).await;
        });
        address
    }

    async fn serve_one_connection(socket: TcpStream, factory: Arc<AuthTestFactory>) {
        let _ = process_socket(socket, None, factory).await;
    }

    fn connection_string(address: std::net::SocketAddr, password: &str) -> String {
        format!(
            "host={} port={} user=proxy_test password={password} dbname=proxy_test sslmode=disable",
            address.ip(),
            address.port()
        )
    }

    #[test]
    fn defaults_to_trust_mode() {
        assert_eq!(
            ProxyAuthConfig::parse("trust", None, None).expect("trust is valid"),
            ProxyAuthConfig::Trust
        );
    }

    #[test]
    fn cleartext_mode_requires_nonempty_credentials() {
        let error = ProxyAuthConfig::parse("cleartext", Some("demo".to_owned()), None)
            .expect_err("password is required");
        assert_eq!(
            error.message,
            "PROXY_AUTH_PASSWORD is required when PROXY_AUTH_MODE=cleartext"
        );
    }

    #[test]
    fn debug_output_redacts_password() {
        let config = ProxyAuthConfig::parse(
            "cleartext",
            Some("demo".to_owned()),
            Some("secret-value".to_owned()),
        )
        .expect("credentials are valid");
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-value"));
    }

    #[tokio::test]
    async fn cleartext_auth_accepts_valid_postgres_driver_credentials() {
        let address = start_auth_server(ProxyAuthConfig::CleartextPassword {
            username: "proxy_test".to_owned(),
            password: "correct-password".to_owned(),
        })
        .await;
        let (client, connection) =
            tokio_postgres::connect(&connection_string(address, "correct-password"), NoTls)
                .await
                .expect("valid PostgreSQL driver credentials must connect");
        let connection_task = tokio::spawn(connection);

        client
            .simple_query("SELECT 1")
            .await
            .expect("authenticated PostgreSQL driver can query");
        drop(client);
        connection_task
            .await
            .expect("join PostgreSQL client connection task")
            .expect("PostgreSQL client connection completes cleanly");
    }

    #[tokio::test]
    async fn cleartext_auth_rejects_invalid_postgres_driver_credentials() {
        let address = start_auth_server(ProxyAuthConfig::CleartextPassword {
            username: "proxy_test".to_owned(),
            password: "correct-password".to_owned(),
        })
        .await;

        let Err(error) =
            tokio_postgres::connect(&connection_string(address, "wrong-password"), NoTls).await
        else {
            panic!("invalid PostgreSQL driver credentials must be rejected");
        };
        assert_eq!(error.code(), Some(&SqlState::INVALID_PASSWORD));
    }
}
