"""HTTP boundary for constrained, side-effect-free ambiguity recommendations."""

from __future__ import annotations

from fastapi import FastAPI, HTTPException

from .contract import (
    CONTRACT_VERSION,
    AmbiguityEvidence,
    AmbiguityKind,
    AmbiguityRequest,
    AmbiguityResponse,
    ObservedShape,
    ObservedType,
    ResolutionCandidate,
    ResolutionAdvisor,
    WriteOperation,
)
from .providers import (
    DeterministicAdvisor,
    ProviderConfigurationError,
    ProviderRequestError,
    provider_advisor,
)


def validate_recommendation(
    request: AmbiguityRequest, recommendation: AmbiguityResponse
) -> AmbiguityResponse:
    """Enforce correlation and the Rust-provided allowlist at the service boundary."""

    if recommendation.schema_profile_version != request.schema_profile_version:
        raise ValueError("recommendation schema profile version does not match the request")
    if recommendation.operation != request.operation:
        raise ValueError("recommendation operation does not match the request")
    if recommendation.target_path != request.target_path:
        raise ValueError("recommendation target path does not match the request")
    if recommendation.candidate not in request.allowed_candidates:
        raise ValueError("recommendation candidate is not in the request allowlist")
    return recommendation


def create_app(advisor: ResolutionAdvisor | None = None) -> FastAPI:
    """Build an app with a constrained provider selected by environment settings."""

    active_advisor = advisor or provider_advisor()
    resolver_app = FastAPI(title="Mongo PG Ambiguity Resolver", version="0.3.0")

    @resolver_app.get("/healthz")
    def health_check() -> dict[str, str]:
        """Provide a dependency-free liveness endpoint."""

        return {"status": "ok"}

    @resolver_app.post("/v1/resolve", response_model=AmbiguityResponse)
    def resolve_ambiguity(request: AmbiguityRequest) -> AmbiguityResponse:
        """Return only an allowlisted recommendation; neither provider can execute MongoDB."""

        try:
            return validate_recommendation(request, active_advisor.recommend(request))
        except ProviderConfigurationError as error:
            raise HTTPException(status_code=503, detail=str(error)) from error
        except ProviderRequestError as error:
            raise HTTPException(status_code=502, detail=str(error)) from error
        except ValueError as error:
            # A malformed provider result is unusable; the Rust caller fails closed.
            raise HTTPException(status_code=422, detail=str(error)) from error

    return resolver_app


app = create_app()


__all__ = [
    "CONTRACT_VERSION",
    "AmbiguityEvidence",
    "AmbiguityKind",
    "AmbiguityRequest",
    "AmbiguityResponse",
    "DeterministicAdvisor",
    "ObservedShape",
    "ObservedType",
    "ResolutionCandidate",
    "ResolutionAdvisor",
    "WriteOperation",
    "create_app",
    "validate_recommendation",
]
