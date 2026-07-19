//! `PostgreSQL` catalog and information-schema emulation.
//!
//! This crate will project versioned schema profiles into the small catalog
//! surface required by supported clients.

/// Name of the `PostgreSQL` information schema namespace.
pub const INFORMATION_SCHEMA: &str = "information_schema";
