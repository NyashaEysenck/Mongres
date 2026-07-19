"""Bounded Google Gemini and OpenAI adapters for ambiguity recommendations."""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

from dotenv import load_dotenv

from .contract import AmbiguityRequest, AmbiguityResponse, Resolution, ResolutionAdvisor


# The repository ignores `.env`; loading it here makes provider credentials
# available to the resolver without putting them in source control.
load_dotenv(override=False)


class ProviderConfigurationError(ValueError):
    """Raised when an enabled model provider lacks safe configuration."""


class ProviderRequestError(RuntimeError):
    """Raised for a model-provider transport or response failure."""


@dataclass(frozen=True)
class ProviderSettings:
    """Runtime configuration for a constrained recommendation provider."""

    provider: str
    timeout_seconds: float
    google_api_key: str | None
    google_model: str
    google_base_url: str
    openai_api_key: str | None
    openai_model: str
    openai_base_url: str

    @classmethod
    def from_environment(cls) -> ProviderSettings:
        """Loads provider configuration without ever returning a secret in an error."""

        timeout_ms = _positive_timeout_ms(os.getenv("AMBIGUITY_LLM_TIMEOUT_MS", "4000"))
        return cls(
            provider=os.getenv("AMBIGUITY_LLM_PROVIDER", "google").lower(),
            timeout_seconds=timeout_ms / 1000,
            google_api_key=os.getenv("GEMINI_API_KEY"),
            google_model=os.getenv("GEMINI_MODEL", "gemini-2.5-flash"),
            google_base_url=os.getenv(
                "GEMINI_API_BASE_URL", "https://generativelanguage.googleapis.com"
            ).rstrip("/"),
            openai_api_key=os.getenv("OPENAI_API_KEY"),
            openai_model=os.getenv("OPENAI_MODEL", "gpt-5.2"),
            openai_base_url=os.getenv("OPENAI_API_BASE_URL", "https://api.openai.com").rstrip(
                "/"
            ),
        )


class DeterministicAdvisor:
    """A side-effect-free fallback for local contract development only."""

    def recommend(self, request: AmbiguityRequest) -> AmbiguityResponse:
        if Resolution.USE_NESTED_PATH in request.allowed_decisions:
            decision = Resolution.USE_NESTED_PATH
            rationale = "Schema evidence permits the deterministic nested-path write."
        else:
            decision = Resolution.REJECT
            rationale = "The requested ambiguity has no safe MVP resolution."
        return AmbiguityResponse(
            schema_profile_version=request.schema_profile_version,
            target_path=request.target_path,
            decision=decision,
            confidence=1.0,
            rationale=rationale,
        )


class EnvironmentAdvisor:
    """Selects Google, OpenAI, or the local fallback from documented environment settings."""

    def __init__(self, settings: ProviderSettings | None = None) -> None:
        self._settings = settings or ProviderSettings.from_environment()

    def recommend(self, request: AmbiguityRequest) -> AmbiguityResponse:
        provider = self._settings.provider
        if provider == "google":
            return GoogleGeminiAdvisor(self._settings).recommend(request)
        if provider == "openai":
            return OpenAIResponsesAdvisor(self._settings).recommend(request)
        if provider == "deterministic":
            return DeterministicAdvisor().recommend(request)
        raise ProviderConfigurationError(
            "AMBIGUITY_LLM_PROVIDER must be google, openai, or deterministic"
        )


class GoogleGeminiAdvisor:
    """Uses Gemini structured output, then validates the response with Pydantic."""

    def __init__(self, settings: ProviderSettings) -> None:
        self._settings = settings

    def recommend(self, request: AmbiguityRequest) -> AmbiguityResponse:
        api_key = self._settings.google_api_key
        if not api_key:
            raise ProviderConfigurationError("GEMINI_API_KEY is required for the google provider")
        model = quote(self._settings.google_model, safe="-._")
        payload = {
            "systemInstruction": {"parts": [{"text": _SYSTEM_INSTRUCTION}]},
            "contents": [{"role": "user", "parts": [{"text": _prompt(request)}]}],
            "generationConfig": {
                "temperature": 0,
                "responseMimeType": "application/json",
                "responseJsonSchema": _response_schema(),
            },
        }
        response = _post_json(
            f"{self._settings.google_base_url}/v1beta/models/{model}:generateContent",
            payload,
            {"x-goog-api-key": api_key},
            self._settings.timeout_seconds,
        )
        return AmbiguityResponse.model_validate_json(_gemini_output_text(response))


class OpenAIResponsesAdvisor:
    """Uses OpenAI Responses structured output, then validates the response with Pydantic."""

    def __init__(self, settings: ProviderSettings) -> None:
        self._settings = settings

    def recommend(self, request: AmbiguityRequest) -> AmbiguityResponse:
        api_key = self._settings.openai_api_key
        if not api_key:
            raise ProviderConfigurationError("OPENAI_API_KEY is required for the openai provider")
        payload = {
            "model": self._settings.openai_model,
            "instructions": _SYSTEM_INSTRUCTION,
            "input": _prompt(request),
            "temperature": 0,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "ambiguity_resolution",
                    "strict": True,
                    "schema": _response_schema(),
                }
            },
        }
        response = _post_json(
            f"{self._settings.openai_base_url}/v1/responses",
            payload,
            {"authorization": f"Bearer {api_key}"},
            self._settings.timeout_seconds,
        )
        return AmbiguityResponse.model_validate_json(_openai_output_text(response))


_SYSTEM_INSTRUCTION = """You are a constrained write-ambiguity advisor.
Return only the JSON object required by the response schema. You may select only
one decision in allowed_decisions and must echo contract_version,
schema_profile_version, and target_path exactly. Never propose MongoDB commands,
aggregation pipelines, operators, paths, or type coercions. Reject when unsure."""


def _prompt(request: AmbiguityRequest) -> str:
    return "Evaluate this non-executable ambiguity request:\n" + json.dumps(
        request.model_dump(mode="json"), separators=(",", ":"), sort_keys=True
    )


def _response_schema() -> dict[str, Any]:
    return {
        "type": "object",
        "additionalProperties": False,
        "required": [
            "contract_version",
            "schema_profile_version",
            "target_path",
            "decision",
            "confidence",
            "rationale",
        ],
        "properties": {
            "contract_version": {"type": "string", "enum": ["v1"]},
            "schema_profile_version": {"type": "integer", "minimum": 1},
            "target_path": {
                "type": "array",
                "minItems": 1,
                "items": {"type": "string"},
            },
            "decision": {"type": "string", "enum": ["use_nested_path", "reject"]},
            "confidence": {"type": "number", "minimum": 0, "maximum": 1},
            # Pydantic applies the length bounds after provider output. Keeping
            # the provider schema to the Gemini-supported JSON Schema subset
            # lets both adapters share one response contract.
            "rationale": {"type": "string"},
        },
    }


def _positive_timeout_ms(raw_value: str) -> int:
    try:
        value = int(raw_value)
    except ValueError as error:
        raise ProviderConfigurationError(
            "AMBIGUITY_LLM_TIMEOUT_MS must be a positive integer"
        ) from error
    if value <= 0:
        raise ProviderConfigurationError("AMBIGUITY_LLM_TIMEOUT_MS must be a positive integer")
    return value


def _post_json(
    url: str, payload: dict[str, Any], headers: dict[str, str], timeout_seconds: float
) -> dict[str, Any]:
    request = Request(
        url,
        data=json.dumps(payload).encode(),
        headers={"content-type": "application/json", **headers},
        method="POST",
    )
    try:
        with urlopen(request, timeout=timeout_seconds) as response:  # noqa: S310
            decoded: object = json.load(response)
    except HTTPError as error:
        raise ProviderRequestError(f"model provider returned HTTP {error.code}") from error
    except (TimeoutError, URLError, OSError, json.JSONDecodeError) as error:
        raise ProviderRequestError(
            "model provider is unavailable or returned invalid JSON"
        ) from error
    if not isinstance(decoded, dict):
        raise ProviderRequestError("model provider returned an unexpected response shape")
    return decoded


def _gemini_output_text(response: dict[str, Any]) -> str:
    try:
        parts = response["candidates"][0]["content"]["parts"]
        text = "".join(part["text"] for part in parts if isinstance(part.get("text"), str))
    except (IndexError, KeyError, TypeError) as error:
        raise ProviderRequestError("Google provider returned no structured candidate") from error
    if not text:
        raise ProviderRequestError("Google provider returned an empty structured candidate")
    return text


def _openai_output_text(response: dict[str, Any]) -> str:
    direct_text = response.get("output_text")
    if isinstance(direct_text, str) and direct_text:
        return direct_text
    try:
        content = response["output"][0]["content"]
        text = "".join(
            item["text"]
            for item in content
            if item.get("type") == "output_text" and isinstance(item.get("text"), str)
        )
    except (IndexError, KeyError, TypeError) as error:
        raise ProviderRequestError("OpenAI provider returned no structured output") from error
    if not text:
        raise ProviderRequestError("OpenAI provider returned an empty structured output")
    return text


def provider_advisor(settings: ProviderSettings | None = None) -> ResolutionAdvisor:
    """Builds the environment-selected provider adapter without exposing provider SDKs."""

    return EnvironmentAdvisor(settings)
