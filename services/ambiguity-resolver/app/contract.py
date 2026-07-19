"""Versioned, non-executable contract shared with the Rust policy crate."""

from __future__ import annotations

from enum import StrEnum
from typing import Literal, Protocol

from pydantic import BaseModel, ConfigDict, Field, model_validator


CONTRACT_VERSION = "v1"


class StrictModel(BaseModel):
    """Reject undeclared fields so executable output has no representation."""

    model_config = ConfigDict(extra="forbid")


class WriteOperation(StrEnum):
    """The only write intents the resolver may inspect."""

    INSERT = "insert"
    UPDATE = "update"
    DELETE = "delete"


class Resolution(StrEnum):
    """The complete MVP decision allowlist shared with the Rust proxy."""

    USE_NESTED_PATH = "use_nested_path"
    REJECT = "reject"


class AmbiguityKind(StrEnum):
    """Schema conditions that prevent an unchecked write."""

    MIXED_BSON_TYPES = "mixed_bson_types"
    MIXED_SHAPES = "mixed_shapes"
    DOTTED_PATH_COLLISION = "dotted_path_collision"
    MISSING_FROM_SAMPLED_DOCUMENTS = "missing_from_sampled_documents"


class ObservedType(StrEnum):
    """The normalized BSON type labels emitted by the Rust policy crate."""

    NULL = "null"
    BOOLEAN = "boolean"
    INTEGER = "integer"
    FLOATING_POINT = "floating_point"
    STRING = "string"
    DATETIME = "datetime"
    OBJECT_ID = "object_id"
    DOCUMENT = "document"
    ARRAY = "array"


class ObservedShape(StrEnum):
    """The normalized structural labels emitted by the Rust policy crate."""

    SCALAR = "scalar"
    DOCUMENT = "document"
    ARRAY = "array"


class AmbiguityEvidence(StrictModel):
    """Minimized, non-document evidence for a single target path."""

    kinds: list[AmbiguityKind] = Field(min_length=1, max_length=4)
    observed_types: list[ObservedType] = Field(min_length=1, max_length=32)
    observed_shapes: list[ObservedShape] = Field(min_length=1, max_length=8)
    missing_documents: int = Field(ge=0)

    @model_validator(mode="after")
    def evidence_is_unique(self) -> AmbiguityEvidence:
        """Avoid accepting redundant evidence that could hide a malformed request."""

        if len(set(self.kinds)) != len(self.kinds):
            raise ValueError("ambiguity kinds must be unique")
        if len(set(self.observed_types)) != len(self.observed_types):
            raise ValueError("observed types must be unique")
        if len(set(self.observed_shapes)) != len(self.observed_shapes):
            raise ValueError("observed shapes must be unique")
        return self


class AmbiguityRequest(StrictModel):
    """A versioned request produced from Rust-calculated schema evidence."""

    contract_version: Literal["v1"] = CONTRACT_VERSION
    schema_profile_version: int = Field(ge=1)
    operation: WriteOperation
    target_path: list[str] = Field(min_length=1, max_length=64)
    ambiguity: AmbiguityEvidence
    allowed_decisions: list[Resolution] = Field(min_length=1, max_length=2)

    @model_validator(mode="after")
    def validate_allowlist_and_path(self) -> AmbiguityRequest:
        """Reject caller-provided decisions that Rust would never issue."""

        if any(not segment or len(segment) > 255 for segment in self.target_path):
            raise ValueError("target path segments must be non-empty and at most 255 characters")
        if len(set(self.allowed_decisions)) != len(self.allowed_decisions):
            raise ValueError("allowed decisions must be unique")
        if Resolution.REJECT not in self.allowed_decisions:
            raise ValueError("allowed decisions must include reject")

        safe_nested_kinds = {AmbiguityKind.MISSING_FROM_SAMPLED_DOCUMENTS}
        if (
            Resolution.USE_NESTED_PATH in self.allowed_decisions
            and (
                set(self.ambiguity.kinds) != safe_nested_kinds
                or len(self.target_path) < 2
            )
        ):
            raise ValueError("use_nested_path is not valid for the supplied ambiguity kinds")
        return self


class AmbiguityResponse(StrictModel):
    """A non-executable, correlation-safe recommendation for Rust to revalidate."""

    contract_version: Literal["v1"] = CONTRACT_VERSION
    schema_profile_version: int = Field(ge=1)
    target_path: list[str] = Field(min_length=1, max_length=64)
    decision: Resolution
    confidence: float = Field(ge=0.0, le=1.0)
    rationale: str = Field(min_length=1, max_length=500)


class ResolutionAdvisor(Protocol):
    """A provider adapter that cannot gain write execution capabilities."""

    def recommend(self, request: AmbiguityRequest) -> AmbiguityResponse:
        """Return only a typed recommendation for the supplied request."""
