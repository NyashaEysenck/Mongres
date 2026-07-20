//! Deterministic write-ambiguity detection and resolver decision validation.
//!
//! This crate consumes typed SQL plans and persisted schema evidence. It never
//! accepts SQL text, raw `MongoDB` commands, or executable model output.

use std::collections::BTreeSet;

use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_schema_discovery::{FieldPath, ObservedShape, ObservedType, SchemaProfile};
use mongo_pg_sql_engine::{Predicate, SqlValue, StatementPlan};
use serde::{Deserialize, Serialize};

pub mod audit;

/// Wire contract version shared by Rust and the Python resolver.
///
/// Version two changes the resolver boundary from an open-ended "decision"
/// to a Rust-owned candidate identifier. The model can select an identifier,
/// but it cannot describe a conversion or an executor operation.
pub const RESOLUTION_CONTRACT_VERSION: &str = "v2";

/// The write operation represented in resolver requests and audit records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOperation {
    Insert,
    Update,
    Delete,
}

/// Why schema evidence is insufficient to execute a write without a policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// The only resolver candidate identifiers that Rust can represent and validate.
///
/// There is deliberately no variant for an aggregation pipeline, `MongoDB`
/// operator, arbitrary field name, or arbitrary type conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionCandidate {
    /// Leave a validated string assignment as a BSON string.
    KeepString,
    /// Convert a canonical integer string to BSON `Int64` in Rust.
    ParseIntegerLosslessly,
    /// Authorize the existing deterministic nested `$set` construction.
    UseNestedPath,
    /// Stop execution before the plan reaches the executor.
    Reject,
}

/// Backward-compatible name for the bounded resolver-candidate type.
///
/// New code should use [`ResolutionCandidate`]. This alias avoids implying
/// that a provider may return an arbitrary policy decision.
pub type ResolverDecision = ResolutionCandidate;

/// A resolver recommendation after Rust has checked its allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedResolution {
    pub field_path: FieldPath,
    pub candidate: ResolutionCandidate,
}

/// Minimized, versioned evidence sent to the resolver for exactly one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolverRequest {
    pub contract_version: String,
    pub schema_profile_version: u32,
    pub operation: WriteOperation,
    pub target_path: Vec<String>,
    pub ambiguity: ResolverAmbiguityEvidence,
    pub allowed_candidates: BTreeSet<ResolutionCandidate>,
}

/// Minimized schema evidence for one resolver target path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolverAmbiguityEvidence {
    pub kinds: BTreeSet<AmbiguityKind>,
    pub observed_types: Vec<String>,
    pub observed_shapes: Vec<String>,
    pub missing_documents: usize,
}

/// The non-executable response format accepted from the resolver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolverResponse {
    pub contract_version: String,
    pub schema_profile_version: u32,
    pub operation: WriteOperation,
    pub target_path: Vec<String>,
    pub candidate: ResolutionCandidate,
    pub confidence: f64,
    pub rationale: String,
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

/// Returns the candidate IDs Rust permits for one typed write and ambiguity.
///
/// The first mixed-type primitive is deliberately narrow: an exact scalar
/// `string`/`integer` field in an `UPDATE` assignment. `KeepString` uses the
/// existing BSON string executor path; `ParseIntegerLosslessly` retags only a
/// canonical signed-decimal string as an `Int64` before that path executes.
/// Shape conflicts, dotted keys, missing sampled fields, filters on the target
/// field, and every other type combination remain reject-only.
#[must_use]
pub fn allowed_candidates(
    plan: &StatementPlan,
    ambiguity: &WriteAmbiguity,
) -> BTreeSet<ResolutionCandidate> {
    let mut candidates = BTreeSet::from([ResolutionCandidate::Reject]);
    let nested_path_is_safe = ambiguity.kinds
        == BTreeSet::from([AmbiguityKind::MissingFromSampledDocuments])
        && !ambiguity.field_path.is_literal_dotted_key()
        && ambiguity.field_path.segments().len() >= 2;
    if nested_path_is_safe {
        candidates.insert(ResolutionCandidate::UseNestedPath);
    }
    if let Some(value) = mixed_type_assignment_value(plan, ambiguity) {
        candidates.insert(ResolutionCandidate::KeepString);
        if parse_integer_losslessly(value).is_some() {
            candidates.insert(ResolutionCandidate::ParseIntegerLosslessly);
        }
    }
    candidates
}

/// Validates one non-executable resolver decision against original evidence.
///
/// # Errors
///
/// Returns an `AmbiguousWrite` error if the decision is not in the Rust
/// allowlist. Callers must treat `Reject` as a request to stop execution.
pub fn validate_resolution(
    plan: &StatementPlan,
    ambiguity: &WriteAmbiguity,
    candidate: ResolutionCandidate,
) -> Result<ValidatedResolution, ProxyError> {
    if !allowed_candidates(plan, ambiguity).contains(&candidate) {
        return Err(ProxyError::new(
            ErrorKind::AmbiguousWrite,
            format!(
                "resolver candidate is not allowed for ambiguous field '{}'",
                ambiguity.field_path.display_name()
            ),
        ));
    }
    Ok(ValidatedResolution {
        field_path: ambiguity.field_path.clone(),
        candidate,
    })
}

/// Builds the minimized request for one detected ambiguity.
#[must_use]
pub fn resolver_request(
    plan: &StatementPlan,
    operation: WriteOperation,
    schema_profile_version: u32,
    ambiguity: &WriteAmbiguity,
) -> ResolverRequest {
    ResolverRequest {
        contract_version: RESOLUTION_CONTRACT_VERSION.to_owned(),
        schema_profile_version,
        operation,
        target_path: ambiguity.field_path.segments().to_vec(),
        ambiguity: ResolverAmbiguityEvidence {
            kinds: ambiguity.kinds.clone(),
            observed_types: ambiguity
                .observed_types
                .iter()
                .map(|observed_type| observed_type_name(*observed_type).to_owned())
                .collect(),
            observed_shapes: ambiguity
                .observed_shapes
                .iter()
                .map(|observed_shape| observed_shape_name(*observed_shape).to_owned())
                .collect(),
            missing_documents: ambiguity.missing_documents,
        },
        allowed_candidates: allowed_candidates(plan, ambiguity),
    }
}

/// Validates a versioned resolver response against its original request.
///
/// # Errors
///
/// Returns an `AmbiguousWrite` error for stale, malformed, low-confidence, or
/// unallowlisted decisions. Such a response must never reach the executor.
pub fn validate_resolver_response(
    request: &ResolverRequest,
    plan: &StatementPlan,
    ambiguity: &WriteAmbiguity,
    response: &ResolverResponse,
    minimum_confidence: f64,
) -> Result<ValidatedResolution, ProxyError> {
    if response.contract_version != RESOLUTION_CONTRACT_VERSION
        || response.schema_profile_version != request.schema_profile_version
        || response.operation != request.operation
        || response.target_path != request.target_path
        || !response.confidence.is_finite()
        || response.confidence < minimum_confidence
        || response.confidence > 1.0
        || response.rationale.is_empty()
        || response.rationale.len() > 500
    {
        return Err(ProxyError::new(
            ErrorKind::AmbiguousWrite,
            "resolver response failed contract, profile, field, confidence, or rationale validation",
        ));
    }
    validate_resolution(plan, ambiguity, response.candidate)
}

/// Applies a validated decision without accepting any executor instructions.
///
/// `UseNestedPath` does not create a pipeline or change an arbitrary path. It
/// authorizes the already parsed nested field path to use the deterministic
/// nested `$set` builder. `Reject` always prevents execution.
///
/// # Errors
///
/// Returns an `AmbiguousWrite` error for rejection, a literal dotted key, a
/// non-nested path, or a path absent from the typed write plan.
pub fn apply_resolution(
    plan: &StatementPlan,
    resolution: &ValidatedResolution,
) -> Result<StatementPlan, ProxyError> {
    match resolution.candidate {
        ResolutionCandidate::Reject => Err(ProxyError::new(
            ErrorKind::AmbiguousWrite,
            format!(
                "resolver selected rejection for ambiguous field '{}'",
                resolution.field_path.display_name()
            ),
        )),
        ResolutionCandidate::UseNestedPath => {
            if resolution.field_path.is_literal_dotted_key()
                || resolution.field_path.segments().len() < 2
                || !write_plan_references_path(plan, &resolution.field_path)
            {
                return Err(ProxyError::new(
                    ErrorKind::AmbiguousWrite,
                    "nested-path resolution does not match a safe parsed write field",
                ));
            }
            Ok(plan.clone())
        }
        ResolutionCandidate::KeepString => {
            mixed_type_assignment_value(
                plan,
                &WriteAmbiguity {
                    field_path: resolution.field_path.clone(),
                    kinds: BTreeSet::from([AmbiguityKind::MixedBsonTypes]),
                    observed_types: BTreeSet::from([ObservedType::Integer, ObservedType::String]),
                    observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
                    missing_documents: 0,
                },
            )
            .ok_or_else(|| {
                ProxyError::new(
                    ErrorKind::AmbiguousWrite,
                    "string-preserving candidate does not match a safe typed assignment",
                )
            })?;
            Ok(plan.clone())
        }
        ResolutionCandidate::ParseIntegerLosslessly => {
            let value = mixed_type_assignment_value(
                plan,
                &WriteAmbiguity {
                    field_path: resolution.field_path.clone(),
                    kinds: BTreeSet::from([AmbiguityKind::MixedBsonTypes]),
                    observed_types: BTreeSet::from([ObservedType::Integer, ObservedType::String]),
                    observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
                    missing_documents: 0,
                },
            )
            .ok_or_else(|| {
                ProxyError::new(
                    ErrorKind::AmbiguousWrite,
                    "integer conversion candidate does not match a safe typed assignment",
                )
            })?;
            let integer = parse_integer_losslessly(value).ok_or_else(|| {
                ProxyError::new(
                    ErrorKind::AmbiguousWrite,
                    "integer conversion candidate requires a canonical signed-decimal string",
                )
            })?;
            retype_update_assignment(plan, &resolution.field_path, integer)
        }
    }
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

fn write_plan_references_path(plan: &StatementPlan, target: &FieldPath) -> bool {
    match plan {
        StatementPlan::Insert(insert) => insert.columns.iter().any(|path| path == target),
        StatementPlan::Update(update) => update
            .assignments
            .iter()
            .any(|assignment| &assignment.path == target),
        StatementPlan::Delete(_) | StatementPlan::Select(_) => false,
    }
}

/// Returns the sole string assignment eligible for the first coercion primitive.
///
/// This is intentionally more restrictive than general SQL writes. A filter on
/// the same mixed-type field could change which BSON values match after a
/// coercion, so it is excluded until its semantics have a dedicated primitive.
fn mixed_type_assignment_value<'a>(
    plan: &'a StatementPlan,
    ambiguity: &WriteAmbiguity,
) -> Option<&'a str> {
    if ambiguity.kinds != BTreeSet::from([AmbiguityKind::MixedBsonTypes])
        || ambiguity.observed_types != BTreeSet::from([ObservedType::Integer, ObservedType::String])
        || ambiguity.observed_shapes != BTreeSet::from([ObservedShape::Scalar])
        || ambiguity.missing_documents != 0
        || ambiguity.field_path.is_literal_dotted_key()
    {
        return None;
    }

    let StatementPlan::Update(update) = plan else {
        return None;
    };
    if predicate_references_path(&update.filter, &ambiguity.field_path) {
        return None;
    }
    let [assignment] = update.assignments.as_slice() else {
        return None;
    };
    if assignment.path != ambiguity.field_path {
        return None;
    }
    let SqlValue::String(value) = &assignment.value else {
        return None;
    };
    Some(value)
}

fn predicate_references_path(predicate: &Predicate, target: &FieldPath) -> bool {
    match predicate {
        Predicate::Compare { path, .. }
        | Predicate::In { path, .. }
        | Predicate::IsNull { path, .. } => path == target,
        Predicate::And(predicates) | Predicate::Or(predicates) => predicates
            .iter()
            .any(|predicate| predicate_references_path(predicate, target)),
    }
}

/// Parses only an integer string whose canonical representation is unchanged.
///
/// Leading zeroes, whitespace, `+` signs, decimal notation, and values outside
/// the `Int64` range are rejected. This keeps the only supported conversion
/// deterministic and makes its boundary visible in the candidate allowlist.
fn parse_integer_losslessly(value: &str) -> Option<i64> {
    let parsed = value.parse::<i64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn retype_update_assignment(
    plan: &StatementPlan,
    target: &FieldPath,
    value: i64,
) -> Result<StatementPlan, ProxyError> {
    let StatementPlan::Update(update) = plan else {
        return Err(ProxyError::new(
            ErrorKind::AmbiguousWrite,
            "integer conversion candidate requires an UPDATE assignment",
        ));
    };
    let [assignment] = update.assignments.as_slice() else {
        return Err(ProxyError::new(
            ErrorKind::AmbiguousWrite,
            "integer conversion candidate requires exactly one assignment",
        ));
    };
    if assignment.path != *target || !matches!(assignment.value, SqlValue::String(_)) {
        return Err(ProxyError::new(
            ErrorKind::AmbiguousWrite,
            "integer conversion candidate does not match the parsed assignment",
        ));
    }

    let mut converted = update.clone();
    converted.assignments[0].value = SqlValue::Integer(value);
    Ok(StatementPlan::Update(converted))
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

fn operation_for_plan(plan: &StatementPlan) -> Result<WriteOperation, ProxyError> {
    match plan {
        StatementPlan::Insert(_) => Ok(WriteOperation::Insert),
        StatementPlan::Update(_) => Ok(WriteOperation::Update),
        StatementPlan::Delete(_) => Ok(WriteOperation::Delete),
        StatementPlan::Select(_) => Err(ProxyError::new(
            ErrorKind::InvalidInput,
            "resolver requests require an INSERT, UPDATE, or DELETE plan",
        )),
    }
}

/// Returns the write operation used in a resolver request.
///
/// # Errors
///
/// Returns an invalid-input error for a read plan.
pub fn write_operation(plan: &StatementPlan) -> Result<WriteOperation, ProxyError> {
    operation_for_plan(plan)
}

fn observed_type_name(observed_type: ObservedType) -> &'static str {
    match observed_type {
        ObservedType::Null => "null",
        ObservedType::Boolean => "boolean",
        ObservedType::Integer => "integer",
        ObservedType::FloatingPoint => "floating_point",
        ObservedType::String => "string",
        ObservedType::DateTime => "datetime",
        ObservedType::ObjectId => "object_id",
        ObservedType::Document => "document",
        ObservedType::Array => "array",
    }
}

fn observed_shape_name(observed_shape: ObservedShape) -> &'static str {
    match observed_shape {
        ObservedShape::Scalar => "scalar",
        ObservedShape::Document => "document",
        ObservedShape::Array => "array",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mongo_pg_schema_discovery::{FieldPath, FieldProfile, ObservedShape};
    use mongo_pg_sql_engine::{AssignmentPlan, Predicate, SqlValue, StatementPlan, UpdatePlan};

    use super::{
        AmbiguityKind, RESOLUTION_CONTRACT_VERSION, ResolutionCandidate, ResolverResponse,
        WriteAmbiguity, WriteOperation, allowed_candidates, apply_resolution,
        detect_write_ambiguities, resolver_request, validate_resolution,
        validate_resolver_response,
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

    fn nested_update_plan() -> StatementPlan {
        let path = FieldPath::top_level("profile").child("city");
        StatementPlan::Update(UpdatePlan {
            collection: "customers".to_owned(),
            assignments: vec![AssignmentPlan {
                path: path.clone(),
                value: SqlValue::String("Harare".to_owned()),
            }],
            filter: Predicate::Compare {
                path,
                operator: mongo_pg_sql_engine::ComparisonOperator::Equal,
                value: SqlValue::String("Bulawayo".to_owned()),
            },
        })
    }

    #[test]
    fn detects_mixed_types_once_per_field_and_rejects_ineligible_assignments() {
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
            allowed_candidates(&update_plan(), &ambiguities[0]),
            BTreeSet::from([ResolutionCandidate::Reject])
        );
    }

    #[test]
    fn permits_only_nested_path_or_rejection_for_a_missing_nested_field() {
        let schema = mongo_pg_schema_discovery::SchemaProfile {
            profile_version: 1,
            sampled_documents: 3,
            fields: vec![FieldProfile {
                path: FieldPath::top_level("profile").child("city"),
                present_documents: 2,
                missing_documents: 1,
                observed_types: BTreeSet::from([mongo_pg_schema_discovery::ObservedType::String]),
                observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
                has_dotted_key_collision: false,
            }],
        };
        let ambiguity = detect_write_ambiguities(&nested_update_plan(), &schema)
            .expect("typed write plan should inspect")
            .pop()
            .expect("missing field should be ambiguous");
        assert!(
            ambiguity
                .kinds
                .contains(&AmbiguityKind::MissingFromSampledDocuments)
        );
        assert!(
            allowed_candidates(&nested_update_plan(), &ambiguity)
                .contains(&ResolutionCandidate::UseNestedPath)
        );
        validate_resolution(
            &nested_update_plan(),
            &ambiguity,
            ResolutionCandidate::UseNestedPath,
        )
        .expect("nested path is allowlisted");
    }

    #[test]
    fn keeps_a_missing_top_level_field_reject_only() {
        let schema = profile(
            BTreeSet::from([mongo_pg_schema_discovery::ObservedType::Integer]),
            1,
            false,
        );
        let ambiguity = detect_write_ambiguities(&update_plan(), &schema)
            .expect("typed write plan should inspect")
            .pop()
            .expect("missing field should be ambiguous");

        assert_eq!(
            allowed_candidates(&update_plan(), &ambiguity),
            BTreeSet::from([ResolutionCandidate::Reject])
        );
    }

    #[test]
    fn validates_and_applies_only_the_echoed_nested_path_candidate() {
        let path = FieldPath::top_level("profile").child("city");
        let ambiguity = WriteAmbiguity {
            field_path: path.clone(),
            kinds: BTreeSet::from([AmbiguityKind::MissingFromSampledDocuments]),
            observed_types: BTreeSet::from([mongo_pg_schema_discovery::ObservedType::String]),
            observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
            missing_documents: 1,
        };
        let plan = StatementPlan::Update(UpdatePlan {
            collection: "customers".to_owned(),
            assignments: vec![AssignmentPlan {
                path,
                value: SqlValue::String("Harare".to_owned()),
            }],
            filter: Predicate::Compare {
                path: FieldPath::top_level("status"),
                operator: mongo_pg_sql_engine::ComparisonOperator::Equal,
                value: SqlValue::Integer(1),
            },
        });
        let request = resolver_request(&plan, WriteOperation::Update, 3, &ambiguity);
        let response = ResolverResponse {
            contract_version: RESOLUTION_CONTRACT_VERSION.to_owned(),
            schema_profile_version: 3,
            operation: WriteOperation::Update,
            target_path: vec!["profile".to_owned(), "city".to_owned()],
            candidate: ResolutionCandidate::UseNestedPath,
            confidence: 0.9,
            rationale: "The path is nested and may be created safely.".to_owned(),
        };
        let resolution = validate_resolver_response(&request, &plan, &ambiguity, &response, 0.8)
            .expect("matching high-confidence response should validate");
        assert_eq!(
            apply_resolution(&plan, &resolution).expect("safe resolution"),
            plan
        );
    }

    #[test]
    fn rejection_stops_before_any_executor_can_receive_a_plan() {
        let ambiguity = WriteAmbiguity {
            field_path: FieldPath::top_level("profile").child("city"),
            kinds: BTreeSet::from([AmbiguityKind::MissingFromSampledDocuments]),
            observed_types: BTreeSet::from([mongo_pg_schema_discovery::ObservedType::String]),
            observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
            missing_documents: 1,
        };
        let resolution = validate_resolution(
            &nested_update_plan(),
            &ambiguity,
            ResolutionCandidate::Reject,
        )
        .expect("reject is an allowlisted stopping decision");

        let error = apply_resolution(&update_plan(), &resolution)
            .expect_err("a rejected decision must not produce an executable plan");
        assert_eq!(error.kind, mongo_pg_common::ErrorKind::AmbiguousWrite);
    }

    #[test]
    fn model_can_select_a_rust_owned_lossless_integer_candidate() {
        let path = FieldPath::top_level("status");
        let plan = StatementPlan::Update(UpdatePlan {
            collection: "customers".to_owned(),
            assignments: vec![AssignmentPlan {
                path: path.clone(),
                value: SqlValue::String("1".to_owned()),
            }],
            filter: Predicate::Compare {
                path: FieldPath::top_level("_id"),
                operator: mongo_pg_sql_engine::ComparisonOperator::Equal,
                value: SqlValue::String("customer-1".to_owned()),
            },
        });
        let ambiguity = WriteAmbiguity {
            field_path: path,
            kinds: BTreeSet::from([AmbiguityKind::MixedBsonTypes]),
            observed_types: BTreeSet::from([
                mongo_pg_schema_discovery::ObservedType::Integer,
                mongo_pg_schema_discovery::ObservedType::String,
            ]),
            observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
            missing_documents: 0,
        };

        let candidates = allowed_candidates(&plan, &ambiguity);
        assert_eq!(
            candidates,
            BTreeSet::from([
                ResolutionCandidate::KeepString,
                ResolutionCandidate::ParseIntegerLosslessly,
                ResolutionCandidate::Reject,
            ])
        );
        let request = resolver_request(&plan, WriteOperation::Update, 9, &ambiguity);
        let response = ResolverResponse {
            contract_version: RESOLUTION_CONTRACT_VERSION.to_owned(),
            schema_profile_version: 9,
            operation: WriteOperation::Update,
            target_path: vec!["status".to_owned()],
            candidate: ResolutionCandidate::ParseIntegerLosslessly,
            confidence: 0.95,
            rationale: "The string is the canonical Int64 representation.".to_owned(),
        };

        let resolution = validate_resolver_response(&request, &plan, &ambiguity, &response, 0.9)
            .expect("Rust-generated candidate should validate");
        let converted = apply_resolution(&plan, &resolution).expect("conversion is deterministic");
        let StatementPlan::Update(converted) = converted else {
            panic!("candidate must preserve the UPDATE plan");
        };
        assert_eq!(converted.assignments[0].value, SqlValue::Integer(1));
    }

    #[test]
    fn rejects_non_canonical_integer_conversion_before_execution() {
        let path = FieldPath::top_level("status");
        let plan = StatementPlan::Update(UpdatePlan {
            collection: "customers".to_owned(),
            assignments: vec![AssignmentPlan {
                path: path.clone(),
                value: SqlValue::String("01".to_owned()),
            }],
            filter: Predicate::Compare {
                path: FieldPath::top_level("_id"),
                operator: mongo_pg_sql_engine::ComparisonOperator::Equal,
                value: SqlValue::String("customer-1".to_owned()),
            },
        });
        let ambiguity = WriteAmbiguity {
            field_path: path,
            kinds: BTreeSet::from([AmbiguityKind::MixedBsonTypes]),
            observed_types: BTreeSet::from([
                mongo_pg_schema_discovery::ObservedType::Integer,
                mongo_pg_schema_discovery::ObservedType::String,
            ]),
            observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
            missing_documents: 0,
        };
        assert_eq!(
            allowed_candidates(&plan, &ambiguity),
            BTreeSet::from([ResolutionCandidate::KeepString, ResolutionCandidate::Reject,])
        );
        assert!(
            validate_resolution(
                &plan,
                &ambiguity,
                ResolutionCandidate::ParseIntegerLosslessly,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_a_response_for_a_different_write_operation() {
        let path = FieldPath::top_level("status");
        let plan = StatementPlan::Update(UpdatePlan {
            collection: "customers".to_owned(),
            assignments: vec![AssignmentPlan {
                path: path.clone(),
                value: SqlValue::String("1".to_owned()),
            }],
            filter: Predicate::Compare {
                path: FieldPath::top_level("_id"),
                operator: mongo_pg_sql_engine::ComparisonOperator::Equal,
                value: SqlValue::String("customer-1".to_owned()),
            },
        });
        let ambiguity = WriteAmbiguity {
            field_path: path,
            kinds: BTreeSet::from([AmbiguityKind::MixedBsonTypes]),
            observed_types: BTreeSet::from([
                mongo_pg_schema_discovery::ObservedType::Integer,
                mongo_pg_schema_discovery::ObservedType::String,
            ]),
            observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
            missing_documents: 0,
        };
        let request = resolver_request(&plan, WriteOperation::Update, 9, &ambiguity);
        let response = ResolverResponse {
            contract_version: RESOLUTION_CONTRACT_VERSION.to_owned(),
            schema_profile_version: 9,
            operation: WriteOperation::Delete,
            target_path: vec!["status".to_owned()],
            candidate: ResolutionCandidate::KeepString,
            confidence: 1.0,
            rationale: "This operation must not be accepted.".to_owned(),
        };

        assert!(
            validate_resolver_response(&request, &plan, &ambiguity, &response, 0.9).is_err()
        );
    }
}
