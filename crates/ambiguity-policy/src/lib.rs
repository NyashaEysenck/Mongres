//! Deterministic write-ambiguity detection and resolver decision validation.
//!
//! This crate consumes typed SQL plans and persisted schema evidence. It never
//! accepts SQL text, raw `MongoDB` commands, or executable model output.

use std::collections::BTreeSet;

use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_schema_discovery::{FieldPath, ObservedShape, ObservedType, SchemaProfile};
use mongo_pg_sql_engine::{Predicate, StatementPlan};

/// Why schema evidence is insufficient to execute a write without a policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AmbiguityKind {
    MixedBsonTypes,
    MixedShapes,
    DottedPathCollision,
    MissingFromSampledDocuments,
}

/// The schema evidence for one ambiguous field used by a proposed write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteAmbiguity {
    pub field_path: FieldPath,
    pub kinds: BTreeSet<AmbiguityKind>,
    pub observed_types: BTreeSet<ObservedType>,
    pub observed_shapes: BTreeSet<ObservedShape>,
    pub missing_documents: usize,
}

/// The only resolver decisions that Rust can represent and validate.
///
/// There is deliberately no variant for an aggregation pipeline, `MongoDB`
/// operator, arbitrary field name, or arbitrary type conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResolverDecision {
    UseNestedPath,
    Reject,
}

/// A resolver recommendation after Rust has checked its allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedResolution {
    pub field_path: FieldPath,
    pub decision: ResolverDecision,
}

/// Finds every field whose observed schema makes a write unsafe to execute directly.
///
/// # Errors
///
/// Returns an invalid-input error when called with a read plan or a plan that
/// refers to a field absent from the supplied profile.
pub fn detect_write_ambiguities(
    plan: &StatementPlan,
    schema: &SchemaProfile,
) -> Result<Vec<WriteAmbiguity>, ProxyError> {
    let mut paths = BTreeSet::new();
    match plan {
        StatementPlan::Insert(insert) => paths.extend(insert.columns.iter().cloned()),
        StatementPlan::Update(update) => {
            paths.extend(
                update
                    .assignments
                    .iter()
                    .map(|assignment| assignment.path.clone()),
            );
            collect_predicate_paths(&update.filter, &mut paths);
        }
        StatementPlan::Delete(delete) => collect_predicate_paths(&delete.filter, &mut paths),
        StatementPlan::Select(_) => {
            return Err(ProxyError::new(
                ErrorKind::InvalidInput,
                "write ambiguity detection requires an INSERT, UPDATE, or DELETE plan",
            ));
        }
    }

    paths
        .into_iter()
        .filter_map(|path| ambiguity_for_path(path, schema).transpose())
        .collect()
}

/// Returns the decisions Rust permits for one ambiguity.
///
/// Mixed scalar types and conflicting document shapes are not coercible in
/// the MVP, so `Reject` is their only safe outcome. The deterministic executor
/// already knows how to construct a nested `$set`, so a missing field or a
/// dotted-key collision may be resolved only as a nested path.
#[must_use]
pub fn allowed_decisions(ambiguity: &WriteAmbiguity) -> BTreeSet<ResolverDecision> {
    let mut decisions = BTreeSet::from([ResolverDecision::Reject]);
    let nested_path_is_safe = ambiguity.kinds.iter().all(|kind| {
        matches!(
            kind,
            AmbiguityKind::DottedPathCollision | AmbiguityKind::MissingFromSampledDocuments
        )
    });
    if nested_path_is_safe {
        decisions.insert(ResolverDecision::UseNestedPath);
    }
    decisions
}

/// Validates one non-executable resolver decision against original evidence.
///
/// # Errors
///
/// Returns an `AmbiguousWrite` error if the decision is not in the Rust
/// allowlist. Callers must treat `Reject` as a request to stop execution.
pub fn validate_resolution(
    ambiguity: &WriteAmbiguity,
    decision: ResolverDecision,
) -> Result<ValidatedResolution, ProxyError> {
    if !allowed_decisions(ambiguity).contains(&decision) {
        return Err(ProxyError::new(
            ErrorKind::AmbiguousWrite,
            format!(
                "resolver decision is not allowed for ambiguous field '{}'",
                ambiguity.field_path.display_name()
            ),
        ));
    }
    Ok(ValidatedResolution {
        field_path: ambiguity.field_path.clone(),
        decision,
    })
}

/// Formats the fail-closed error used before a validated resolution exists.
#[must_use]
pub fn unresolved_write_error(ambiguities: &[WriteAmbiguity]) -> ProxyError {
    let fields = ambiguities
        .iter()
        .map(|ambiguity| ambiguity.field_path.display_name())
        .collect::<Vec<_>>()
        .join(", ");
    ProxyError::new(
        ErrorKind::AmbiguousWrite,
        format!("write requires a validated ambiguity resolution for field(s): {fields}"),
    )
}

fn collect_predicate_paths(predicate: &Predicate, paths: &mut BTreeSet<FieldPath>) {
    match predicate {
        Predicate::Compare { path, .. }
        | Predicate::In { path, .. }
        | Predicate::IsNull { path, .. } => {
            paths.insert(path.clone());
        }
        Predicate::And(predicates) | Predicate::Or(predicates) => {
            for predicate in predicates {
                collect_predicate_paths(predicate, paths);
            }
        }
    }
}

fn ambiguity_for_path(
    path: FieldPath,
    schema: &SchemaProfile,
) -> Result<Option<WriteAmbiguity>, ProxyError> {
    let profile = schema.field(&path).ok_or_else(|| {
        ProxyError::new(
            ErrorKind::InvalidInput,
            format!(
                "field '{}' is not present in the active schema profile",
                path.display_name()
            ),
        )
    })?;
    let non_null_type_count = profile
        .observed_types
        .iter()
        .filter(|observed_type| **observed_type != ObservedType::Null)
        .count();
    let mut kinds = BTreeSet::new();
    if non_null_type_count > 1 {
        kinds.insert(AmbiguityKind::MixedBsonTypes);
    }
    if profile.observed_shapes.len() > 1 {
        kinds.insert(AmbiguityKind::MixedShapes);
    }
    if profile.has_dotted_key_collision {
        kinds.insert(AmbiguityKind::DottedPathCollision);
    }
    if profile.missing_documents > 0 {
        kinds.insert(AmbiguityKind::MissingFromSampledDocuments);
    }
    Ok((!kinds.is_empty()).then_some(WriteAmbiguity {
        field_path: path,
        kinds,
        observed_types: profile.observed_types.clone(),
        observed_shapes: profile.observed_shapes.clone(),
        missing_documents: profile.missing_documents,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mongo_pg_schema_discovery::{FieldPath, FieldProfile, ObservedShape};
    use mongo_pg_sql_engine::{AssignmentPlan, Predicate, SqlValue, StatementPlan, UpdatePlan};

    use super::{
        AmbiguityKind, ResolverDecision, allowed_decisions, detect_write_ambiguities,
        validate_resolution,
    };

    fn profile(
        observed_types: BTreeSet<mongo_pg_schema_discovery::ObservedType>,
        missing_documents: usize,
        has_dotted_key_collision: bool,
    ) -> mongo_pg_schema_discovery::SchemaProfile {
        mongo_pg_schema_discovery::SchemaProfile {
            profile_version: 1,
            sampled_documents: 3,
            fields: vec![FieldProfile {
                path: FieldPath::top_level("status"),
                present_documents: 3 - missing_documents,
                missing_documents,
                observed_types,
                observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
                has_dotted_key_collision,
            }],
        }
    }

    fn update_plan() -> StatementPlan {
        StatementPlan::Update(UpdatePlan {
            collection: "customers".to_owned(),
            assignments: vec![AssignmentPlan {
                path: FieldPath::top_level("status"),
                value: SqlValue::Integer(2),
            }],
            filter: Predicate::Compare {
                path: FieldPath::top_level("status"),
                operator: mongo_pg_sql_engine::ComparisonOperator::Equal,
                value: SqlValue::Integer(1),
            },
        })
    }

    #[test]
    fn detects_mixed_types_once_per_field_and_allows_only_rejection() {
        let schema = profile(
            BTreeSet::from([
                mongo_pg_schema_discovery::ObservedType::Integer,
                mongo_pg_schema_discovery::ObservedType::String,
            ]),
            0,
            false,
        );
        let ambiguities = detect_write_ambiguities(&update_plan(), &schema)
            .expect("typed write plan should inspect");
        assert_eq!(ambiguities.len(), 1);
        assert!(
            ambiguities[0]
                .kinds
                .contains(&AmbiguityKind::MixedBsonTypes)
        );
        assert_eq!(
            allowed_decisions(&ambiguities[0]),
            BTreeSet::from([ResolverDecision::Reject])
        );
    }

    #[test]
    fn permits_only_nested_path_or_rejection_for_missing_or_dotted_fields() {
        let schema = profile(
            BTreeSet::from([mongo_pg_schema_discovery::ObservedType::Integer]),
            1,
            true,
        );
        let ambiguity = detect_write_ambiguities(&update_plan(), &schema)
            .expect("typed write plan should inspect")
            .pop()
            .expect("missing field should be ambiguous");
        assert!(
            ambiguity
                .kinds
                .contains(&AmbiguityKind::DottedPathCollision)
        );
        assert!(
            ambiguity
                .kinds
                .contains(&AmbiguityKind::MissingFromSampledDocuments)
        );
        assert!(allowed_decisions(&ambiguity).contains(&ResolverDecision::UseNestedPath));
        validate_resolution(&ambiguity, ResolverDecision::UseNestedPath)
            .expect("nested path is allowlisted");
    }
}
