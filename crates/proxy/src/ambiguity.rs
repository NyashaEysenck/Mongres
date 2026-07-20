//! Write-ambiguity orchestration at the proxy boundary.
//!
//! This module coordinates detection, a bounded resolver call, strict response
//! validation, and redacted auditing. It never accepts executable model output.

use std::{collections::BTreeSet, sync::Mutex};

use mongo_pg_ambiguity_policy::{
    ResolutionCandidate, WriteAmbiguity, apply_resolution,
    audit::{
        AmbiguityAuditRecord, AuditFailure, AuditOperation, AuditOutcome, AuditWriteCounts,
        RedactedAmbiguity, ResolverConfidence,
    },
    detect_write_ambiguities, resolver_request, unresolved_write_error, validate_resolver_response,
    write_operation,
};
use mongo_pg_common::{ErrorKind, ProxyError};
use mongo_pg_mongo_executor::WriteOutcome;
use mongo_pg_resolver_client::ResolverClient;
use mongo_pg_schema_discovery::SchemaProfile;
use mongo_pg_sql_engine::StatementPlan;

/// A typed write plan after any required policy decision has been validated.
#[derive(Debug)]
pub(crate) struct ResolvedWrite {
    pub(crate) plan: StatementPlan,
    pub(crate) audit_context: Option<AuditContext>,
}

/// Redacted state carried from a validated decision until executor completion.
#[derive(Debug)]
pub(crate) struct AuditContext {
    operation: AuditOperation,
    ambiguities: Vec<RedactedAmbiguity>,
    allowed_candidates: BTreeSet<ResolutionCandidate>,
    selected_candidate: ResolutionCandidate,
    confidence: ResolverConfidence,
}

/// Resolves the one explicitly supported ambiguity without changing a typed plan.
///
/// Clear writes return without contacting the resolver. Any ambiguity outside
/// the Rust allowlist, malformed response, timeout, or rejected decision is
/// recorded and returned as a fail-closed error.
pub(crate) async fn resolve_write_plan(
    plan: StatementPlan,
    schema: &SchemaProfile,
    resolver: &ResolverClient,
    minimum_confidence: f64,
    audit_records: &Mutex<Vec<AmbiguityAuditRecord>>,
) -> Result<ResolvedWrite, ProxyError> {
    let ambiguities = detect_write_ambiguities(&plan, schema)?;
    if ambiguities.is_empty() {
        return Ok(ResolvedWrite {
            plan,
            audit_context: None,
        });
    }

    let operation = write_operation(&plan)?;
    if ambiguities.len() != 1 {
        record_blocked(
            schema,
            audit_operation(operation),
            &ambiguities,
            BTreeSet::from([ResolutionCandidate::Reject]),
            AuditFailure::NoSafeResolution,
            audit_records,
        );
        return Err(unresolved_write_error(&ambiguities));
    }

    resolve_single_ambiguity(
        plan,
        schema,
        resolver,
        minimum_confidence,
        audit_records,
        operation,
        ambiguities,
    )
    .await
}

async fn resolve_single_ambiguity(
    plan: StatementPlan,
    schema: &SchemaProfile,
    resolver: &ResolverClient,
    minimum_confidence: f64,
    audit_records: &Mutex<Vec<AmbiguityAuditRecord>>,
    operation: mongo_pg_ambiguity_policy::WriteOperation,
    ambiguities: Vec<WriteAmbiguity>,
) -> Result<ResolvedWrite, ProxyError> {
    let ambiguity = &ambiguities[0];
    let request = resolver_request(&plan, operation, schema.profile_version, ambiguity);
    if !has_executable_candidate(&request.allowed_candidates) {
        record_blocked(
            schema,
            audit_operation(operation),
            &ambiguities,
            request.allowed_candidates,
            AuditFailure::NoSafeResolution,
            audit_records,
        );
        return Err(unresolved_write_error(&ambiguities));
    }

    let response = match resolver.resolve(&request).await {
        Ok(response) => response,
        Err(error) => {
            record_blocked(
                schema,
                audit_operation(operation),
                &ambiguities,
                request.allowed_candidates,
                AuditFailure::ResolverUnavailable,
                audit_records,
            );
            return Err(error);
        }
    };
    let confidence = ResolverConfidence::from_ratio(response.confidence);
    let resolution =
        match validate_resolver_response(&request, &plan, ambiguity, &response, minimum_confidence)
        {
            Ok(resolution) => resolution,
            Err(error) => {
                record_blocked(
                    schema,
                    audit_operation(operation),
                    &ambiguities,
                    request.allowed_candidates.clone(),
                    validation_failure(&request, &response, minimum_confidence),
                    audit_records,
                );
                return Err(error);
            }
        };
    let resolved_plan = match apply_resolution(&plan, &resolution) {
        Ok(resolved_plan) => resolved_plan,
        Err(error) => {
            record_blocked(
                schema,
                audit_operation(operation),
                &ambiguities,
                request.allowed_candidates.clone(),
                AuditFailure::ResolverRejected,
                audit_records,
            );
            return Err(error);
        }
    };
    let confidence = confidence.ok_or_else(|| {
        ProxyError::new(
            ErrorKind::AmbiguousWrite,
            "validated resolver response has an invalid confidence value",
        )
    })?;

    Ok(ResolvedWrite {
        plan: resolved_plan,
        audit_context: Some(AuditContext {
            operation: audit_operation(operation),
            ambiguities: ambiguities.iter().map(RedactedAmbiguity::from).collect(),
            allowed_candidates: request.allowed_candidates,
            selected_candidate: resolution.candidate,
            confidence,
        }),
    })
}

fn has_executable_candidate(candidates: &BTreeSet<ResolutionCandidate>) -> bool {
    candidates.iter().any(|candidate| {
        matches!(
            candidate,
            ResolutionCandidate::UseNestedPath
                | ResolutionCandidate::KeepString
                | ResolutionCandidate::ParseIntegerLosslessly
        )
    })
}

/// Records a completed deterministic write if ambiguity resolution was used.
pub(crate) fn record_execution(
    schema: &SchemaProfile,
    context: Option<&AuditContext>,
    outcome: WriteOutcome,
    audit_records: &Mutex<Vec<AmbiguityAuditRecord>>,
) {
    let Some(context) = context else {
        return;
    };
    record(
        audit_records,
        AmbiguityAuditRecord::new(
            schema.profile_version,
            context.operation,
            context.ambiguities.clone(),
            context.allowed_candidates.clone(),
            Some(context.selected_candidate),
            Some(context.confidence),
            AuditOutcome::Executed(AuditWriteCounts {
                matched: outcome.matched,
                modified: outcome.modified,
                inserted: outcome.inserted,
                deleted: outcome.deleted,
            }),
        ),
    );
}

/// Records a deterministic executor failure after a resolution was accepted.
pub(crate) fn record_mongo_execution_failure(
    schema: &SchemaProfile,
    context: Option<&AuditContext>,
    audit_records: &Mutex<Vec<AmbiguityAuditRecord>>,
) {
    let Some(context) = context else {
        return;
    };
    record(
        audit_records,
        AmbiguityAuditRecord::new(
            schema.profile_version,
            context.operation,
            context.ambiguities.clone(),
            context.allowed_candidates.clone(),
            Some(context.selected_candidate),
            Some(context.confidence),
            AuditOutcome::Blocked(AuditFailure::MongoExecutionFailed),
        ),
    );
}

fn record_blocked(
    schema: &SchemaProfile,
    operation: AuditOperation,
    ambiguities: &[WriteAmbiguity],
    allowed_candidates: BTreeSet<ResolutionCandidate>,
    failure: AuditFailure,
    audit_records: &Mutex<Vec<AmbiguityAuditRecord>>,
) {
    record(
        audit_records,
        AmbiguityAuditRecord::new(
            schema.profile_version,
            operation,
            ambiguities.iter().map(RedactedAmbiguity::from),
            allowed_candidates,
            None,
            None,
            AuditOutcome::Blocked(failure),
        ),
    );
}

fn record(audit_records: &Mutex<Vec<AmbiguityAuditRecord>>, record: AmbiguityAuditRecord) {
    if let Ok(mut records) = audit_records.lock() {
        records.push(record);
    }
}

fn audit_operation(operation: mongo_pg_ambiguity_policy::WriteOperation) -> AuditOperation {
    match operation {
        mongo_pg_ambiguity_policy::WriteOperation::Insert => AuditOperation::Insert,
        mongo_pg_ambiguity_policy::WriteOperation::Update => AuditOperation::Update,
        mongo_pg_ambiguity_policy::WriteOperation::Delete => AuditOperation::Delete,
    }
}

fn validation_failure(
    request: &mongo_pg_ambiguity_policy::ResolverRequest,
    response: &mongo_pg_ambiguity_policy::ResolverResponse,
    minimum_confidence: f64,
) -> AuditFailure {
    if response.schema_profile_version != request.schema_profile_version {
        AuditFailure::StaleSchemaProfile
    } else if !response.confidence.is_finite()
        || response.confidence < minimum_confidence
        || response.confidence > 1.0
    {
        AuditFailure::LowConfidence
    } else {
        AuditFailure::InvalidResolverResponse
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, time::Duration};

    use super::resolve_write_plan;
    use mongo_pg_resolver_client::{ResolverClient, ResolverClientConfig};
    use mongo_pg_schema_discovery::{
        FieldPath, FieldProfile, ObservedShape, ObservedType, SchemaProfile,
    };
    use mongo_pg_sql_engine::{
        AssignmentPlan, ComparisonOperator, Predicate, SqlValue, StatementPlan, UpdatePlan,
    };

    fn field(path: FieldPath, present_documents: usize, missing_documents: usize) -> FieldProfile {
        FieldProfile {
            path,
            present_documents,
            missing_documents,
            observed_types: BTreeSet::from([ObservedType::String]),
            observed_shapes: BTreeSet::from([ObservedShape::Scalar]),
            has_dotted_key_collision: false,
        }
    }

    fn schema(fields: Vec<FieldProfile>) -> SchemaProfile {
        SchemaProfile {
            profile_version: 7,
            sampled_documents: 2,
            fields,
        }
    }

    fn update(path: FieldPath) -> StatementPlan {
        StatementPlan::Update(UpdatePlan {
            collection: "customers".to_owned(),
            assignments: vec![AssignmentPlan {
                path,
                value: SqlValue::String("Harare".to_owned()),
            }],
            filter: Predicate::Compare {
                path: FieldPath::top_level("name"),
                operator: ComparisonOperator::Equal,
                value: SqlValue::String("Amina".to_owned()),
            },
        })
    }

    fn resolver(endpoint: String) -> ResolverClient {
        let config = ResolverClientConfig::new(endpoint, Duration::from_secs(1));
        ResolverClient::new(&config).expect("test resolver endpoint is valid")
    }

    #[tokio::test]
    async fn clear_write_bypasses_the_resolver() {
        let schema = schema(vec![
            field(FieldPath::top_level("name"), 2, 0),
            field(FieldPath::top_level("status"), 2, 0),
        ]);
        let records = std::sync::Mutex::new(Vec::new());

        let resolved = resolve_write_plan(
            update(FieldPath::top_level("status")),
            &schema,
            // The endpoint is deliberately unavailable. Success proves this
            // clear write returned before the resolver transport was invoked.
            &resolver("http://127.0.0.1:1/v1/resolve".to_owned()),
            0.8,
            &records,
        )
        .await
        .expect("clear write resolves without service involvement");

        assert!(resolved.audit_context.is_none());
        assert!(
            records
                .lock()
                .expect("audit records are available")
                .is_empty()
        );
    }
}

#[cfg(test)]
mod live_tests;
