//! SQL parsing, validation, and typed-plan boundaries.
//!
//! Parsing with `sqlparser-rs` will be added in a later milestone. This crate
//! already owns the statement support contract so the protocol and executor do
//! not need to make independent SQL decisions.

use mongo_pg_common::{ErrorKind, ProxyError};

/// Statement families understood by the first release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Select,
    Insert,
    Update,
    Delete,
}

/// Rejects a statement family not yet included in the support contract.
///
/// # Errors
///
/// Returns a feature-not-supported error when a future statement family is
/// outside the MVP contract.
pub fn require_supported(kind: StatementKind) -> Result<(), ProxyError> {
    match kind {
        StatementKind::Select
        | StatementKind::Insert
        | StatementKind::Update
        | StatementKind::Delete => Ok(()),
    }
}

/// Constructs the standard error used for SQL features outside the MVP.
#[must_use]
pub fn unsupported_feature(feature: &str) -> ProxyError {
    ProxyError::new(
        ErrorKind::FeatureNotSupported,
        format!("SQL feature is not supported: {feature}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{StatementKind, require_supported};

    #[test]
    fn accepts_mvp_statement_families() {
        for kind in [
            StatementKind::Select,
            StatementKind::Insert,
            StatementKind::Update,
            StatementKind::Delete,
        ] {
            assert!(require_supported(kind).is_ok());
        }
    }
}
