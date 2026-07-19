//! Shared domain types and error conventions for the proxy workspace.

/// A stable, internal classification for errors that cross crate boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// SQL syntax is invalid.
    Syntax,
    /// SQL is valid but unsupported by the current implementation.
    FeatureNotSupported,
    /// An identifier, type, or statement is semantically invalid.
    InvalidInput,
    /// A request would be unsafe to execute without further resolution.
    AmbiguousWrite,
    /// `MongoDB` rejected an operation.
    Database,
    /// A dependent service failed or timed out.
    Dependency,
}

impl ErrorKind {
    /// Returns the `PostgreSQL` SQLSTATE used at the protocol boundary.
    #[must_use]
    pub const fn sql_state(self) -> &'static str {
        match self {
            Self::Syntax => "42601",
            Self::FeatureNotSupported => "0A000",
            Self::InvalidInput => "22023",
            Self::AmbiguousWrite => "22000",
            Self::Database => "XX000",
            Self::Dependency => "08006",
        }
    }
}

/// A user-safe error to be translated into a `PostgreSQL` error response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyError {
    /// Stable error classification.
    pub kind: ErrorKind,
    /// Safe diagnostic text for the client.
    pub message: String,
}

impl ProxyError {
    /// Creates a client-safe proxy error.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorKind;

    #[test]
    fn maps_unsupported_features_to_postgres_sqlstate() {
        assert_eq!(ErrorKind::FeatureNotSupported.sql_state(), "0A000");
    }
}
