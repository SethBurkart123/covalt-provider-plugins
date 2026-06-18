from __future__ import annotations

import logging
import re
from typing import Any

import httpx

from backend.providers import get_base_url
from backend.providers.oauth_plugin_entry import (
    agent_options_from_resolve,
    create_oauth_provider_entry,
    manifest_llm,
    manifest_system_blocks,
)
from backend.providers.options import resolve_common_options

from .api.provider import supports_adaptive_reasoning, supports_reasoning

ANTHROPIC_VERSION = "2023-06-01"
ANTHROPIC_OAUTH_BETA = (
    "claude-code-20250219,oauth-2025-04-20,"
    "fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14"
)
CLAUDE_CODE_SYSTEM = "You are Claude Code, Anthropic's official CLI for Claude."
logger = logging.getLogger(__name__)


def _oauth_manager():
    from backend.services.models.provider_oauth_manager import get_provider_oauth_manager

    return get_provider_oauth_manager()


def _get_anthropic_credentials() -> dict[str, Any]:
    creds = _oauth_manager().get_valid_credentials(
        "anthropic_oauth",
        refresh_if_missing_expiry=True,
        allow_stale_on_refresh_failure=False,
    )
    if not creds:
        raise RuntimeError("Anthropic OAuth not connected in Settings.")
    return creds


def _normalize_reasoning_effort(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    if normalized in {"none", "off"}:
        return "none"
    if normalized == "xhigh":
        return "max"
    if normalized in {"auto", "minimal", "low", "medium", "high", "max"}:
        return normalized
    return None


def resolve_options(
    model_id: str,
    model_options: dict[str, Any] | None,
    node_params: dict[str, Any] | None,
) -> dict[str, Any]:
    resolved = resolve_common_options(model_options, node_params)
    effort = _normalize_reasoning_effort((model_options or {}).get("reasoning_effort"))
    if effort and effort not in {"none", "auto"} and supports_reasoning(model_id):
        resolved["reasoning_effort"] = effort
    return resolved


def _parse_reasoning_levels(value: Any) -> list[dict[str, str]]:
    if not isinstance(value, list):
        return []

    parsed: list[dict[str, str]] = []
    seen_efforts: set[str] = set()
    for item in value:
        effort: str | None = None
        description: str | None = None
        if isinstance(item, dict):
            effort_value = item.get("effort") or item.get("value")
            if isinstance(effort_value, str):
                effort = effort_value.strip().lower()
            description_value = item.get("description")
            if isinstance(description_value, str) and description_value.strip():
                description = description_value.strip()
        elif isinstance(item, str):
            effort = item.strip().lower()
        if not effort or effort in seen_efforts:
            continue
        seen_efforts.add(effort)
        level: dict[str, str] = {"effort": effort}
        if description:
            level["description"] = description
        parsed.append(level)
    return parsed


def _default_reasoning_levels(model_id: str) -> list[dict[str, str]]:
    if not supports_reasoning(model_id):
        return []
    efforts = ["low", "medium", "high"]
    if supports_adaptive_reasoning(model_id):
        efforts.append("max")
    return [{"effort": effort} for effort in efforts]


def get_model_options(
    model_id: str,
    model_metadata: dict[str, Any] | None = None,
) -> dict[str, Any]:
    metadata = model_metadata or {}
    reasoning_levels = _parse_reasoning_levels(metadata.get("supported_reasoning_levels"))
    if not reasoning_levels:
        reasoning_levels = _default_reasoning_levels(model_id)
    if not reasoning_levels:
        return {"main": [], "advanced": []}

    options = [
        {
            "value": level["effort"],
            "label": level.get("description") or level["effort"],
        }
        for level in reasoning_levels
    ]
    allowed_efforts = {option["value"] for option in options}
    if "auto" not in allowed_efforts:
        options.insert(0, {"value": "auto", "label": "auto"})

    default_effort = metadata.get("default_reasoning_level")
    if not isinstance(default_effort, str) or default_effort.strip().lower() not in allowed_efforts:
        default_effort = "auto"

    return {
        "main": [
            {
                "key": "reasoning_effort",
                "label": "Reasoning Effort",
                "type": "select",
                "default": default_effort,
                "options": options,
            }
        ],
        "advanced": [],
    }


async def fetch_models() -> list[dict[str, Any]]:
    try:
        creds = _get_anthropic_credentials()
    except RuntimeError:
        return []
    access_token = creds.get("access_token")
    if not access_token:
        return []

    try:
        async with httpx.AsyncClient(timeout=10) as client:
            response = await client.get(
                "https://api.anthropic.com/v1/models",
                headers={
                    "Authorization": f"Bearer {access_token}",
                    "anthropic-version": ANTHROPIC_VERSION,
                    "anthropic-beta": ANTHROPIC_OAUTH_BETA,
                },
            )
            if response.is_success:
                models = response.json().get("data", [])
                results: list[dict[str, Any]] = []
                for model in models:
                    if not isinstance(model, dict):
                        continue
                    model_id = str(model.get("id") or "").strip()
                    if not model_id:
                        continue
                    results.append(
                        {
                            "id": model_id,
                            "name": model.get("display_name", model_id),
                            "supported_reasoning_levels": _default_reasoning_levels(model_id),
                            "default_reasoning_level": "auto",
                        }
                    )
                return results
    except Exception as exc:
        logger.warning("[anthropic_oauth] Failed to fetch models: %s", exc)

    from backend.services.models_dev import fetch_models_dev_provider

    fallback = await fetch_models_dev_provider(
        "anthropic",
        predicate=lambda _id, info: info.get("tool_call") is True,
    )
    results = []
    for model in fallback:
        model_id = model.get("id")
        if not isinstance(model_id, str) or not model_id:
            continue
        results.append(
            {
                "id": model_id,
                "name": model.get("name", model_id),
                "supported_reasoning_levels": _default_reasoning_levels(model_id),
                "default_reasoning_level": "auto",
            }
        )
    return results


async def test_connection() -> tuple[bool, str | None]:
    try:
        _get_anthropic_credentials()
    except RuntimeError:
        return False, "OAuth not connected"
    return True, None


def resolve_agent_stream_config(
    *,
    model_id: str,
    model_options: dict[str, Any] | None,
    manifest: dict[str, Any],
    **_kwargs: Any,
) -> dict[str, Any]:
    creds = _get_anthropic_credentials()
    access_token = creds.get("access_token")
    if not access_token:
        raise RuntimeError("Anthropic OAuth credentials are incomplete.")

    llm = manifest_llm(manifest)
    headers = dict(llm["headers"])
    headers["Authorization"] = f"Bearer {access_token}"
    config: dict[str, Any] = {
        "providerId": "anthropic_oauth",
        "model": model_id,
        "provider": {"dialect": llm["dialect"]},
        "baseUrl": get_base_url("anthropic_oauth") or llm["base_url"],
        "headers": headers,
        "systemBlocks": manifest_system_blocks(manifest, default_text=CLAUDE_CODE_SYSTEM),
    }
    options = agent_options_from_resolve(resolve_options, model_id, model_options)
    if options:
        config["options"] = options
    return config


def create_provider(
    provider_id: str,
    manifest: dict[str, Any] | None = None,
    **_kwargs: Any,
) -> dict[str, Any]:
    from .oauth import oauth_handlers

    manifest_data = manifest or {}

    def resolve_stream_config(**kwargs: Any) -> dict[str, Any]:
        return resolve_agent_stream_config(manifest=manifest_data, **kwargs)

    return create_oauth_provider_entry(
        manifest=manifest_data,
        resolve_stream_config=resolve_stream_config,
        fetch_models=fetch_models,
        test_connection=test_connection,
        get_model_options=get_model_options,
        resolve_options=resolve_options,
        oauth=oauth_handlers(),
    )
