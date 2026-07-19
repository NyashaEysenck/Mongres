"""Contract tests for the constrained ambiguity resolver."""

from __future__ import annotations

import unittest

from app.main import (
    AmbiguityEvidence,
    AmbiguityKind,
    AmbiguityRequest,
    AmbiguityResponse,
    ObservedShape,
    ObservedType,
    Resolution,
    WriteOperation,
    validate_recommendation,
)
from pydantic import ValidationError


def nested_path_request() -> AmbiguityRequest:
    return AmbiguityRequest(
        schema_profile_version=7,
        operation=WriteOperation.UPDATE,
        target_path=["profile", "city"],
        ambiguity=AmbiguityEvidence(
            kinds=[AmbiguityKind.MISSING_FROM_SAMPLED_DOCUMENTS],
            observed_types=[ObservedType.STRING],
            observed_shapes=[ObservedShape.SCALAR],
            missing_documents=2,
        ),
        allowed_decisions=[Resolution.USE_NESTED_PATH, Resolution.REJECT],
    )


class ResolverContractTests(unittest.TestCase):
    def test_use_nested_path_requires_safe_ambiguity_kinds(self) -> None:
        with self.assertRaises(ValidationError):
            AmbiguityRequest(
                schema_profile_version=1,
                operation=WriteOperation.UPDATE,
                target_path=["status"],
                ambiguity=AmbiguityEvidence(
                    kinds=[AmbiguityKind.MIXED_BSON_TYPES],
                    observed_types=[ObservedType.INTEGER, ObservedType.STRING],
                    observed_shapes=[ObservedShape.SCALAR],
                    missing_documents=0,
                ),
                allowed_decisions=[Resolution.USE_NESTED_PATH, Resolution.REJECT],
            )

    def test_use_nested_path_requires_a_nested_target_path(self) -> None:
        with self.assertRaises(ValidationError):
            AmbiguityRequest(
                schema_profile_version=1,
                operation=WriteOperation.UPDATE,
                target_path=["status"],
                ambiguity=AmbiguityEvidence(
                    kinds=[AmbiguityKind.MISSING_FROM_SAMPLED_DOCUMENTS],
                    observed_types=[ObservedType.STRING],
                    observed_shapes=[ObservedShape.SCALAR],
                    missing_documents=1,
                ),
                allowed_decisions=[Resolution.USE_NESTED_PATH, Resolution.REJECT],
            )

    def test_recommendation_cannot_change_target_or_profile_version(self) -> None:
        request = nested_path_request()
        recommendation = AmbiguityResponse(
            schema_profile_version=8,
            target_path=request.target_path,
            decision=Resolution.USE_NESTED_PATH,
            confidence=1.0,
            rationale="irrelevant",
        )
        with self.assertRaises(ValueError):
            validate_recommendation(request, recommendation)

    def test_recommendation_cannot_escape_the_rust_allowlist(self) -> None:
        request = nested_path_request()
        request = request.model_copy(update={"allowed_decisions": [Resolution.REJECT]})
        recommendation = AmbiguityResponse(
            schema_profile_version=request.schema_profile_version,
            target_path=request.target_path,
            decision=Resolution.USE_NESTED_PATH,
            confidence=1.0,
            rationale="safe",
        )
        with self.assertRaises(ValueError):
            validate_recommendation(request, recommendation)

    def test_extra_fields_are_rejected(self) -> None:
        payload = nested_path_request().model_dump()
        for forbidden_field in ("pipeline", "operator", "mongo_path", "coercion"):
            with self.subTest(forbidden_field=forbidden_field):
                unsafe_payload = payload | {forbidden_field: "untrusted"}
                with self.assertRaises(ValidationError):
                    AmbiguityRequest.model_validate(unsafe_payload)


if __name__ == "__main__":
    unittest.main()
