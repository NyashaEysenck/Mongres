//! Redacted audit data for the write-ambiguity resolution boundary.
//!
//! These types are intentionally data-only. They retain the policy evidence
//! needed to explain a decision without retaining SQL text, BSON values,
//! credentials, resolver rationale, or driver error strings.

use std::collections::BTreeSet;

use super::{AmbiguityKind, ResolutionCandidate, WriteAmbiguity};

/// The kind of write for which an ambiguity decision was considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOperation {
    Insert,
    Update,
    Delete,
}

/// One ambiguous field, stripped of observed values and document contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedAmbiguity {
    /// The schema field path, which is metadata rather than a document value.
    pub field_path: String,
    /// The schema conditions which made the field unsafe to write directly.
    pub kinds: BTreeSet<AmbiguityKind>,
}

impl From<&WriteAmbiguity> for RedactedAmbiguity {
    fn from(ambiguity: &WriteAmbiguity) -> Self {
        Self {
            field_path: ambiguity.field_path.display_name(),
            kinds: ambiguity.kinds.clone(),
        }
    }
}

/// A validated resolver confidence stored as basis points, from 0 to 10,000.
///
/// The integer representation avoids recording invalid floating-point values
/// such as `NaN` and has an unambiguous boundary representation for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolverConfidence(u16);

impl ResolverConfidence {
    /// The highest representable confidence: 100%.
    pub const MAX_BASIS_POINTS: u16 = 10_000;

    /// Creates a confidence value from basis points.
    #[must_use]
    pub const fn from_basis_points(value: u16) -> Option<Self> {
        if value <= Self::MAX_BASIS_POINTS {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Creates a confidence value from a resolver ratio in the closed interval
    /// from 0.0 to 1.0.
    #[must_use]
    pub fn from_ratio(value: f64) -> Option<Self> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return None;
        }
        let basis_points = (value * f64::from(Self::MAX_BASIS_POINTS))
            .round()
            .to_string()
            .parse()
            .ok()?;
        Self::from_basis_points(basis_points)
    }

    /// Returns the stored confidence in basis points.
    #[must_use]
    pub const fn basis_points(self) -> u16 {
        self.0
    }
}

/// Result counts safe to retain after a completed deterministic write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuditWriteCounts {
    pub matched: u64,
    pub modified: u64,
    pub inserted: u64,
    pub deleted: u64,
}

/// A redacted reason why no write was executed or a write did not complete.
///
/// This deliberately excludes dependency, model, and `MongoDB` error messages;
/// those messages may contain sensitive values and belong in separately
/// controlled operational telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditFailure {
    /// Rust could not construct a safe allowlist for the detected evidence.
    NoSafeResolution,
    ResolverRejected,
    ResolverUnavailable,
    ResolverTimedOut,
    InvalidResolverResponse,
    StaleSchemaProfile,
    LowConfidence,
    MongoExecutionFailed,
}

/// The final recorded state of an ambiguity-gated write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    /// The deterministic executor completed the write and supplied counts.
    Executed(AuditWriteCounts),
    /// Execution was prevented by a known fail-closed condition.
    Blocked(AuditFailure),
}

/// A complete redacted record for one ambiguity-gated write attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguityAuditRecord {
    /// Version of the persisted schema profile used to make the decision.
    pub schema_profile_version: u32,
    pub operation: AuditOperation,
    /// Field-level ambiguity metadata only; no sampled BSON values are kept.
    pub ambiguities: Vec<RedactedAmbiguity>,
    /// Candidate IDs computed by Rust before contacting the resolver.
    pub allowed_candidates: BTreeSet<ResolutionCandidate>,
    /// The allowlisted candidate selected for this attempt, if a response was accepted.
    pub selected_candidate: Option<ResolutionCandidate>,
    /// The accepted resolver confidence, if a response was accepted.
    pub confidence: Option<ResolverConfidence>,
    pub outcome: AuditOutcome,
}

impl AmbiguityAuditRecord {
    /// Builds an audit record from already-redacted policy inputs.
    #[must_use]
    pub fn new(
        schema_profile_version: u32,
        operation: AuditOperation,
        ambiguities: impl IntoIterator<Item = RedactedAmbiguity>,
        allowed_candidates: BTreeSet<ResolutionCandidate>,
        selected_candidate: Option<ResolutionCandidate>,
        confidence: Option<ResolverConfidence>,
        outcome: AuditOutcome,
    ) -> Self {
        Self {
            schema_profile_version,
            operation,
            ambiguities: ambiguities.into_iter().collect(),
            allowed_candidates,
            selected_candidate,
            confidence,
            outcome,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mongo_pg_schema_discovery::{FieldPath, ObservedShape, ObservedType};

    use super::{
        AmbiguityAuditRecord, AuditOperation, AuditOutcome, AuditWriteCounts, RedactedAmbiguity,
        ResolverConfidence,
    };
    use crate::{AmbiguityKind, ResolutionCandidate, WriteAmbiguity};

    #[test]
    fn redacted_ambiguity_keeps_policy_metadata_but_not_schema_values() {
        let ambiguity = WriteAmbiguity {
            field_path: FieldPath::top_level("profile").child("city"),
            kinds: BTreeSet::from([AmbiguityKind::MissingFromSampledDocuments]),
            observed_types: BTreeSet::from([ObservedType::String]),
            observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
            missing_documents: 2,
        };

        let redacted = RedactedAmbiguity::from(&ambiguity);

        assert_eq!(redacted.field_path, "profile.city");
        assert_eq!(
            redacted.kinds,
            BTreeSet::from([AmbiguityKind::MissingFromSampledDocuments])
        );
        let debug = format!("{redacted:?}");
        assert!(!debug.contains("String"));
        assert!(!debug.contains("Scalar"));
        assert!(!debug.contains("missing_documents"));
    }

    #[test]
    fn audit_record_captures_the_required_redacted_decision_lifecycle() {
        let confidence = ResolverConfidence::from_basis_points(9_500).expect("valid confidence");
        let record = AmbiguityAuditRecord::new(
            7,
            AuditOperation::Update,
            [RedactedAmbiguity {
                field_path: "profile.city".to_owned(),
                kinds: BTreeSet::from([AmbiguityKind::MissingFromSampledDocuments]),
            }],
            BTreeSet::from([
                ResolutionCandidate::Reject,
                ResolutionCandidate::UseNestedPath,
            ]),
            Some(ResolutionCandidate::UseNestedPath),
            Some(confidence),
            AuditOutcome::Executed(AuditWriteCounts {
                matched: 1,
                modified: 1,
                inserted: 0,
                deleted: 0,
            }),
        );

        assert_eq!(record.schema_profile_version, 7);
        assert_eq!(record.operation, AuditOperation::Update);
        assert_eq!(record.ambiguities[0].field_path, "profile.city");
        assert!(
            record
                .allowed_candidates
                .contains(&ResolutionCandidate::UseNestedPath)
        );
        assert_eq!(
            record.selected_candidate,
            Some(ResolutionCandidate::UseNestedPath)
        );
        assert_eq!(
            record.confidence.expect("stored confidence").basis_points(),
            9_500
        );
        assert_eq!(
            record.outcome,
            AuditOutcome::Executed(AuditWriteCounts {
                matched: 1,
                modified: 1,
                inserted: 0,
                deleted: 0,
            })
        );
    }

    #[test]
    fn confidence_rejects_values_outside_the_closed_interval() {
        assert!(ResolverConfidence::from_basis_points(0).is_some());
        assert!(
            ResolverConfidence::from_basis_points(ResolverConfidence::MAX_BASIS_POINTS).is_some()
        );
        assert!(
            ResolverConfidence::from_basis_points(ResolverConfidence::MAX_BASIS_POINTS + 1)
                .is_none()
        );
        assert_eq!(
            ResolverConfidence::from_ratio(0.95)
                .expect("valid ratio")
                .basis_points(),
            9_500
        );
        assert!(ResolverConfidence::from_ratio(f64::NAN).is_none());
        assert!(ResolverConfidence::from_ratio(1.01).is_none());
    }
}
