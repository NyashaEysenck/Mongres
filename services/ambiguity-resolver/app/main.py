"""HTTP boundary for constrained write-ambiguity recommendations.

This service deliberately has no MongoDB driver dependency. The Rust proxy owns
schema validation and every database operation.
"""

from enum import StrEnum

from fastapi import FastAPI
from pydantic import BaseModel, Field


class Resolution(StrEnum):
    """Decisions the Rust proxy may independently validate."""

    USE_NESTED_PATH = "use_nested_path"
    USE_LITERAL_KEY = "use_literal_key"
    COERCE_TO_STRING = "coerce_to_string"
    REJECT = "reject"


class AmbiguityRequest(BaseModel):
    """Minimized schema evidence sent by the proxy for one proposed write."""

    operation: str = Field(pattern="^(insert|update|delete)$")
    field_path: str = Field(min_length=1)
    observed_shapes: list[str] = Field(min_length=1)
    allowed_decisions: list[Resolution] = Field(min_length=1)


class AmbiguityResponse(BaseModel):
    """Non-executable recommendation returned to the Rust proxy."""

    decision: Resolution
    confidence: float = Field(ge=0.0, le=1.0)
    rationale: str = Field(min_length=1, max_length=500)


app = FastAPI(title="Mongo PG Ambiguity Resolver", version="0.1.0")


@app.get("/healthz")
def health_check() -> dict[str, str]:
    """Provide a dependency-free liveness endpoint."""

    return {"status": "ok"}

