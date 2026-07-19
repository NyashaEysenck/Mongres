//! Deterministic `MongoDB` execution boundary.
//!
//! This crate will be the only location permitted to construct `MongoDB` driver
//! operations. It accepts typed plans, not SQL strings or raw LLM output.

/// Counts returned from a completed `MongoDB` write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    pub matched: u64,
    pub modified: u64,
    pub inserted: u64,
    pub deleted: u64,
}
