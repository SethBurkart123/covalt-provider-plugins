from __future__ import annotations

import asyncio
from typing import Any
from urllib.parse import urlencode

import httpx

from backend.services.oauth.oauth_shared import AUTH_TIMEOUT_S, CALLBACK_TIMEOUT_S, expires_at_from_seconds, generate_pkce
from backend.services.models.provider_oauth_manager import OAuthHostBridge


def _decode_client_id() -> str:
    return bytes.fromhex("39643163323530612d653631622d343464392d383865642d353934346431393632663565").decode()


async def start_oauth(flow: Any, host: OAuthHostBridge, _options: dict[str, Any] | None = None) -> None:
    client_id = _decode_client_id()
    verifier, challenge = generate_pkce()
    flow.verifier = verifier
    flow.state = verifier
    auth_params = {
        "code": "true",
        "client_id": client_id,
        "response_type": "code",
        "redirect_uri": "https://console.anthropic.com/oauth/code/callback",
        "scope": "org:create_api_key user:profile user:inference",
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "state": verifier,
    }
    flow.auth_url = f"https://claude.ai/oauth/authorize?{urlencode(auth_params)}"
    flow.instructions = "Paste the authorization code"
    host.register_code_future(flow)
    host.spawn_finish_task(flow, _finish_oauth(flow, host))


async def _finish_oauth(flow: Any, host: OAuthHostBridge) -> None:
    try:
        if not flow.code_future:
            raise ValueError("Missing authorization code")
        code, state = await asyncio.wait_for(flow.code_future, timeout=AUTH_TIMEOUT_S)
        if flow.state and state and state != flow.state:
            raise ValueError("State mismatch")
        data = {
            "grant_type": "authorization_code",
            "client_id": _decode_client_id(),
            "code": code,
            "state": state,
            "redirect_uri": "https://console.anthropic.com/oauth/code/callback",
            "code_verifier": flow.verifier,
        }
        async with httpx.AsyncClient(timeout=20.0) as client:
            resp = await client.post(
                "https://console.anthropic.com/v1/oauth/token",
                json=data,
                headers={"Content-Type": "application/json"},
            )
        if resp.status_code != 200:
            raise ValueError(resp.text)
        payload = resp.json()
        access_token = payload.get("access_token")
        refresh_token = payload.get("refresh_token")
        expires_in = payload.get("expires_in")
        if not access_token or not refresh_token or not isinstance(expires_in, int):
            raise ValueError("Invalid token response")
        host.save_tokens(
            flow,
            access_token=access_token,
            refresh_token=refresh_token,
            expires_at=expires_at_from_seconds(expires_in),
        )
    except Exception as exc:
        host.fail_flow(flow, str(exc))
    finally:
        host.clear_pending_callback(flow)


def refresh_credentials(refresh_token: str, extra: dict[str, Any] | None) -> dict[str, Any] | None:
    _ = extra
    payload = {
        "grant_type": "refresh_token",
        "client_id": _decode_client_id(),
        "refresh_token": refresh_token,
    }
    with httpx.Client(timeout=20.0) as client:
        resp = client.post(
            "https://console.anthropic.com/v1/oauth/token",
            json=payload,
            headers={"Content-Type": "application/json"},
        )
    if resp.status_code != 200:
        return None
    data = resp.json()
    access_token = data.get("access_token")
    new_refresh = data.get("refresh_token") or refresh_token
    expires_in = data.get("expires_in")
    if not access_token or not isinstance(expires_in, int):
        return None
    expires_at = expires_at_from_seconds(expires_in)
    from backend.db import db_session
    from backend.db.provider_oauth import save_provider_oauth

    with db_session() as sess:
        save_provider_oauth(
            sess,
            provider="anthropic_oauth",
            access_token=access_token,
            refresh_token=new_refresh,
            token_type="Bearer",
            expires_at=expires_at,
        )
    return {
        "access_token": access_token,
        "refresh_token": new_refresh,
        "expires_at": expires_at,
    }


def oauth_handlers() -> dict[str, Any]:
    return {
        "start_oauth": start_oauth,
        "refresh_credentials": refresh_credentials,
    }
