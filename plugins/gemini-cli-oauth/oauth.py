from __future__ import annotations

import asyncio
import os
from typing import Any
from urllib.parse import urlencode

import httpx

from backend.services.oauth.oauth_shared import CALLBACK_TIMEOUT_S, expires_at_from_seconds, generate_pkce
from backend.services.models.provider_oauth_manager import OAuthHostBridge

_ENV_CLIENT_ID = "GEMINI_CLI_OAUTH_CLIENT_ID"
_ENV_CLIENT_SECRET = "GEMINI_CLI_OAUTH_CLIENT_SECRET"


def _client_credentials() -> tuple[str, str]:
    client_id = os.environ.get(_ENV_CLIENT_ID, "").strip()
    client_secret = os.environ.get(_ENV_CLIENT_SECRET, "").strip()
    if not client_id or not client_secret:
        raise RuntimeError(
            f"Set {_ENV_CLIENT_ID} and {_ENV_CLIENT_SECRET} "
            "(public Gemini CLI installed-app values; see plugin README)."
        )
    return client_id, client_secret


async def start_oauth(flow: Any, host: OAuthHostBridge) -> None:
    client_id, _ = _client_credentials()
    verifier, challenge = generate_pkce()
    flow.verifier = verifier
    flow.state = verifier
    callback_port = 8085
    callback_path = "/oauth2callback"
    callback_redirect_uri = host.create_callback_redirect_uri(callback_port, callback_path)
    params = {
        "client_id": client_id,
        "response_type": "code",
        "redirect_uri": callback_redirect_uri,
        "scope": " ".join(
            [
                "https://www.googleapis.com/auth/cloud-platform",
                "https://www.googleapis.com/auth/userinfo.email",
                "https://www.googleapis.com/auth/userinfo.profile",
            ]
        ),
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "state": verifier,
        "access_type": "offline",
        "prompt": "consent",
    }
    flow.auth_url = f"https://accounts.google.com/o/oauth2/v2/auth?{urlencode(params)}"
    flow.instructions = "Complete the sign-in in your browser"
    flow.extra["callback_redirect_uri"] = callback_redirect_uri
    host.register_code_future(flow)
    flow.callback_server = host.start_callback_server(
        flow,
        port=callback_port,
        path=callback_path,
    )
    host.spawn_finish_task(flow, _finish_oauth(flow, host))


async def _finish_oauth(flow: Any, host: OAuthHostBridge) -> None:
    callback_redirect_uri = str(
        flow.extra.get("callback_redirect_uri")
        or host.create_callback_redirect_uri(8085, "/oauth2callback")
    )
    try:
        if not flow.code_future:
            raise ValueError("Missing authorization code")
        code, state = await asyncio.wait_for(flow.code_future, timeout=CALLBACK_TIMEOUT_S)
        if flow.state and state and state != flow.state:
            raise ValueError("State mismatch")
        token = await _exchange_token(
            code=code,
            verifier=flow.verifier,
            redirect_uri=callback_redirect_uri,
        )
        email = await _get_user_email(token["access_token"])
        project_id = await _discover_project(token["access_token"])
        host.save_tokens(
            flow,
            access_token=token["access_token"],
            refresh_token=token["refresh_token"],
            expires_at=expires_at_from_seconds(token["expires_in"]),
            extra={"projectId": project_id, "email": email},
        )
    except Exception as exc:
        host.fail_flow(flow, str(exc))
    finally:
        host.clear_pending_callback(flow)
        if flow.callback_server:
            flow.callback_server.stop()


async def _exchange_token(*, code: str, verifier: str | None, redirect_uri: str) -> dict[str, Any]:
    client_id, client_secret = _client_credentials()
    data = {
        "client_id": client_id,
        "client_secret": client_secret,
        "code": code,
        "grant_type": "authorization_code",
        "redirect_uri": redirect_uri,
        "code_verifier": verifier,
    }
    async with httpx.AsyncClient(timeout=20.0) as client:
        resp = await client.post(
            "https://oauth2.googleapis.com/token",
            data=data,
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
    if resp.status_code != 200:
        raise ValueError(resp.text)
    payload = resp.json()
    if not payload.get("refresh_token"):
        raise ValueError("No refresh token received")
    return payload


async def _get_user_email(access_token: str) -> str | None:
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(
            "https://www.googleapis.com/oauth2/v1/userinfo?alt=json",
            headers={"Authorization": f"Bearer {access_token}"},
        )
    if resp.status_code != 200:
        return None
    email = resp.json().get("email")
    return email if isinstance(email, str) else None


async def _discover_project(access_token: str) -> str:
    env_project = os.environ.get("GOOGLE_CLOUD_PROJECT") or os.environ.get("GOOGLE_CLOUD_PROJECT_ID")
    headers = {
        "Authorization": f"Bearer {access_token}",
        "Content-Type": "application/json",
        "User-Agent": "google-api-nodejs-client/9.15.1",
        "X-Goog-Api-Client": "gl-node/22.17.0",
    }
    async with httpx.AsyncClient(timeout=20.0) as client:
        load_resp = await client.post(
            "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist",
            headers=headers,
            json={
                "cloudaicompanionProject": env_project,
                "metadata": {
                    "ideType": "IDE_UNSPECIFIED",
                    "platform": "PLATFORM_UNSPECIFIED",
                    "pluginType": "GEMINI",
                    "duetProject": env_project,
                },
            },
        )
    data = load_resp.json() if load_resp.status_code == 200 else None
    current_tier = data.get("currentTier") if isinstance(data, dict) else None
    if current_tier:
        project = data.get("cloudaicompanionProject") if isinstance(data, dict) else None
        if project:
            return project
        if env_project:
            return env_project
        raise ValueError("Missing GOOGLE_CLOUD_PROJECT or GOOGLE_CLOUD_PROJECT_ID for this account")

    allowed = data.get("allowedTiers") if isinstance(data, dict) else None
    tier_id = "legacy-tier"
    if isinstance(allowed, list):
        default_tier = next(
            (tier for tier in allowed if isinstance(tier, dict) and tier.get("isDefault")),
            None,
        )
        if default_tier and default_tier.get("id"):
            tier_id = default_tier["id"]

    if tier_id != "free-tier" and not env_project:
        raise ValueError("Missing GOOGLE_CLOUD_PROJECT or GOOGLE_CLOUD_PROJECT_ID for this account")

    onboard_body: dict[str, Any] = {
        "tierId": tier_id,
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
        },
    }
    if tier_id != "free-tier" and env_project:
        onboard_body["cloudaicompanionProject"] = env_project
        onboard_body["metadata"]["duetProject"] = env_project

    async with httpx.AsyncClient(timeout=20.0) as client:
        onboard_resp = await client.post(
            "https://cloudcode-pa.googleapis.com/v1internal:onboardUser",
            headers=headers,
            json=onboard_body,
        )
        if onboard_resp.status_code != 200:
            raise ValueError(onboard_resp.text)
        lro = onboard_resp.json()
        if not lro.get("done") and lro.get("name"):
            lro = await _poll_operation(lro["name"], headers)

    if isinstance(lro, dict):
        response = lro.get("response")
        if isinstance(response, dict):
            project = response.get("cloudaicompanionProject", {}).get("id")
            if project:
                return project
    if env_project:
        return env_project
    raise ValueError("Could not provision a project")


async def _poll_operation(name: str, headers: dict[str, str]) -> dict[str, Any]:
    async with httpx.AsyncClient(timeout=20.0) as client:
        while True:
            resp = await client.get(
                f"https://cloudcode-pa.googleapis.com/v1internal/{name}",
                headers=headers,
            )
            if resp.status_code != 200:
                raise ValueError(resp.text)
            data = resp.json()
            if data.get("done"):
                return data
            await asyncio.sleep(5)


def refresh_credentials(refresh_token: str, extra: dict[str, Any] | None) -> dict[str, Any] | None:
    project_id = extra.get("projectId") if isinstance(extra, dict) else None
    if not project_id:
        return None
    try:
        client_id, client_secret = _client_credentials()
    except RuntimeError:
        return None
    with httpx.Client(timeout=20.0) as client:
        resp = client.post(
            "https://oauth2.googleapis.com/token",
            data={
                "client_id": client_id,
                "client_secret": client_secret,
                "refresh_token": refresh_token,
                "grant_type": "refresh_token",
            },
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
    if resp.status_code != 200:
        return None
    payload = resp.json()
    access_token = payload.get("access_token")
    expires_in = payload.get("expires_in")
    if not access_token or not isinstance(expires_in, int):
        return None
    expires_at = expires_at_from_seconds(expires_in)
    saved_extra = {"projectId": project_id}
    from backend.db import db_session
    from backend.db.provider_oauth import save_provider_oauth

    with db_session() as sess:
        save_provider_oauth(
            sess,
            provider="google_gemini_cli",
            access_token=access_token,
            refresh_token=refresh_token,
            token_type="Bearer",
            expires_at=expires_at,
            extra=saved_extra,
        )
    return {
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_at": expires_at,
        "extra": saved_extra,
    }


def oauth_handlers() -> dict[str, Any]:
    return {
        "start_oauth": start_oauth,
        "refresh_credentials": refresh_credentials,
    }
