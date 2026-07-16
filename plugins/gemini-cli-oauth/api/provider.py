from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import os
import secrets
from pathlib import Path
from typing import Any
from urllib.parse import urlencode

import httpx
from covalt.provider import Auth, Provider, Transport
from covalt.provider.context import ProviderContext

DEFAULT_ENDPOINT = "https://cloudcode-pa.googleapis.com"
CALLBACK_PATH = "/oauth2callback"
CALLBACK_PORT = 8085

_ENV_CLIENT_ID = "GEMINI_CLI_OAUTH_CLIENT_ID"
_ENV_CLIENT_SECRET = "GEMINI_CLI_OAUTH_CLIENT_SECRET"

_GEMINI_HEADERS = {
    "content-type": "application/json",
    "accept": "text/event-stream",
    "user-agent": "google-cloud-sdk vscode_cloudshelleditor/0.1",
    "x-goog-api-client": "gl-node/22.17.0",
    "client-metadata": json.dumps(
        {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
        }
    ),
}


def _client_credentials() -> tuple[str, str]:
    client_id = os.environ.get(_ENV_CLIENT_ID, "").strip()
    client_secret = os.environ.get(_ENV_CLIENT_SECRET, "").strip()
    if not client_id or not client_secret:
        raise RuntimeError(
            f"Set {_ENV_CLIENT_ID} and {_ENV_CLIENT_SECRET} "
            "(public Gemini CLI installed-app values; see plugin README)."
        )
    return client_id, client_secret


def _pkce() -> tuple[str, str]:
    verifier = base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode()
    challenge = base64.urlsafe_b64encode(
        hashlib.sha256(verifier.encode()).digest()
    ).rstrip(b"=").decode()
    return verifier, challenge


def _project_id_path(data_dir: str | None) -> Path | None:
    if not data_dir:
        return None
    return Path(data_dir) / "project_id"


def _save_project_id(data_dir: str | None, project_id: str) -> None:
    path = _project_id_path(data_dir)
    if path is None:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(project_id.strip(), encoding="utf-8")


def _load_project_id(data_dir: str | None) -> str | None:
    path = _project_id_path(data_dir)
    if path is None or not path.is_file():
        return None
    value = path.read_text(encoding="utf-8").strip()
    return value or None


def _callback_redirect_uri(ctx: ProviderContext) -> str:
    if ctx.callback_uri:
        return ctx.callback_uri
    port = ctx.callback_port or CALLBACK_PORT
    return f"http://127.0.0.1:{port}{CALLBACK_PATH}"


async def _exchange_token(*, code: str, verifier: str, redirect_uri: str) -> dict[str, Any]:
    client_id, client_secret = _client_credentials()
    async with httpx.AsyncClient(timeout=20.0) as client:
        resp = await client.post(
            "https://oauth2.googleapis.com/token",
            data={
                "client_id": client_id,
                "client_secret": client_secret,
                "code": code,
                "grant_type": "authorization_code",
                "redirect_uri": redirect_uri,
                "code_verifier": verifier,
            },
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
    if resp.status_code != 200:
        raise ValueError(resp.text)
    payload = resp.json()
    if not payload.get("refresh_token"):
        raise ValueError("No refresh token received")
    return payload


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


class GeminiCliOAuth(Provider):
    id = "google-gemini-cli"
    name = "Gemini CLI OAuth"

    def transport(self, ctx: ProviderContext, model: str) -> Transport | dict[str, Any]:
        project_id = _load_project_id(ctx.data_dir)
        if not project_id:
            return Transport(
                dialect="google-code-assist",
                base_url=DEFAULT_ENDPOINT,
                headers=dict(_GEMINI_HEADERS),
            )
        return {
            "dialect": "google-code-assist",
            "baseUrl": DEFAULT_ENDPOINT,
            "headers": dict(_GEMINI_HEADERS),
            "projectId": project_id,
        }

    async def login(self, ctx: ProviderContext) -> Auth:
        client_id, _ = _client_credentials()
        verifier, challenge = _pkce()
        redirect_uri = _callback_redirect_uri(ctx)
        params = {
            "client_id": client_id,
            "response_type": "code",
            "redirect_uri": redirect_uri,
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
        auth_url = f"https://accounts.google.com/o/oauth2/v2/auth?{urlencode(params)}"
        await ctx.ui.show(note="Complete the sign-in in your browser", url=auth_url)
        callback = await ctx.ui.browser(auth_url)
        code = str(callback.get("code") or "").strip()
        state = str(callback.get("state") or "").strip()
        if not code:
            raise ValueError("Missing authorization code")
        if state and state != verifier:
            raise ValueError("State mismatch")
        token = await _exchange_token(code=code, verifier=verifier, redirect_uri=redirect_uri)
        project_id = await _discover_project(token["access_token"])
        _save_project_id(ctx.data_dir, project_id)
        expires_in = token.get("expires_in")
        if not isinstance(expires_in, int):
            raise ValueError("Invalid token response")
        return Auth(
            access=str(token["access_token"]),
            refresh=str(token["refresh_token"]),
            expires_in=expires_in,
        )

    async def refresh(self, ctx: ProviderContext, auth: Auth) -> Auth:
        if not _load_project_id(ctx.data_dir):
            raise ValueError("Missing project id")
        refresh_token = (auth.refresh or "").strip()
        if not refresh_token:
            raise ValueError("Missing refresh token")
        client_id, client_secret = _client_credentials()
        async with httpx.AsyncClient(timeout=20.0) as client:
            resp = await client.post(
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
            raise ValueError(resp.text)
        payload = resp.json()
        access_token = payload.get("access_token")
        expires_in = payload.get("expires_in")
        if not access_token or not isinstance(expires_in, int):
            raise ValueError("Invalid token response")
        return Auth(
            access=str(access_token),
            refresh=refresh_token,
            expires_in=expires_in,
        )


PLUGIN = GeminiCliOAuth()
