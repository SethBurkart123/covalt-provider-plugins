from __future__ import annotations

import json
from typing import Any

from backend.providers import get_base_url
from backend.providers.oauth_plugin_entry import (
    create_oauth_provider_entry,
    manifest_llm,
)

DEFAULT_ENDPOINT = "https://cloudcode-pa.googleapis.com"


def _oauth_manager():
    from backend.services.models.provider_oauth_manager import get_provider_oauth_manager

    return get_provider_oauth_manager()


def _get_gemini_cli_credentials() -> dict[str, Any]:
    creds = _oauth_manager().get_valid_credentials(
        "google_gemini_cli",
        refresh_if_missing_expiry=True,
        allow_stale_on_refresh_failure=False,
    )
    if not creds:
        raise RuntimeError("Google Gemini CLI OAuth not connected in Settings.")
    return creds


def _project_id(creds: dict[str, Any]) -> str:
    extra = creds.get("extra") or {}
    project_id = extra.get("projectId") if isinstance(extra, dict) else None
    if not isinstance(project_id, str) or not project_id.strip():
        raise RuntimeError("Google Gemini CLI OAuth credentials are incomplete.")
    return project_id.strip()


async def fetch_models() -> list[dict[str, str]]:
    try:
        _get_gemini_cli_credentials()
    except RuntimeError:
        return []

    from backend.services.models_dev import fetch_models_dev_provider

    return await fetch_models_dev_provider(
        "google",
        predicate=lambda model_id, info: info.get("tool_call") is True
        and model_id.startswith("gemini-"),
    )


async def test_connection() -> tuple[bool, str | None]:
    try:
        _get_gemini_cli_credentials()
    except RuntimeError:
        return False, "OAuth not connected"
    return True, None


def resolve_agent_stream_config(
    *,
    model_id: str,
    manifest: dict[str, Any],
    **_kwargs: Any,
) -> dict[str, Any]:
    creds = _get_gemini_cli_credentials()
    access_token = creds.get("access_token")
    if not isinstance(access_token, str) or not access_token:
        raise RuntimeError("Google Gemini CLI OAuth credentials are incomplete.")

    llm = manifest_llm(manifest)
    headers = dict(llm["headers"])
    headers["Authorization"] = f"Bearer {access_token}"
    if "client-metadata" in headers and isinstance(headers["client-metadata"], str):
        try:
            json.loads(headers["client-metadata"])
        except json.JSONDecodeError:
            pass

    return {
        "providerId": "google_gemini_cli",
        "model": model_id,
        "provider": {
            "dialect": llm["dialect"],
            "projectId": _project_id(creds),
        },
        "baseUrl": get_base_url("google_gemini_cli") or llm["base_url"] or DEFAULT_ENDPOINT,
        "headers": headers,
    }


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
        oauth=oauth_handlers(),
    )
