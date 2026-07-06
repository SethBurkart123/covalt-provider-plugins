from __future__ import annotations

import base64
import hashlib
import re
import secrets
from typing import Any
from urllib.parse import urlencode

import httpx
from covalt.provider import Auth, Field, Provider, Transport
from covalt.provider.context import ProviderContext

AUTHORIZE_URL = "https://claude.ai/oauth/authorize"
TOKEN_URL = "https://console.anthropic.com/v1/oauth/token"
REDIRECT_URI = "https://console.anthropic.com/oauth/code/callback"
SCOPE = "org:create_api_key user:profile user:inference"
CLAUDE_SYSTEM = "You are Claude Code, Anthropic's official CLI for Claude."

ANTHROPIC_HEADERS = {
    "anthropic-version": "2023-06-01",
    "anthropic-beta": "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14",
    "content-type": "application/json",
    "accept": "application/json",
    "user-agent": "claude-cli/2.1.2 (external, cli)",
    "x-app": "cli",
}

REASONING_BUDGETS = {
    "minimal": 1024,
    "low": 2048,
    "medium": 8192,
    "high": 16384,
    "max": 32000,
    "xhigh": 32000,
}


def _decode_client_id() -> str:
    return bytes.fromhex(
        "39643163323530612d653631622d343464392d383865642d353934346431393632663565"
    ).decode()


def _pkce() -> tuple[str, str]:
    verifier = base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode()
    challenge = base64.urlsafe_b64encode(
        hashlib.sha256(verifier.encode()).digest()
    ).rstrip(b"=").decode()
    return verifier, challenge


def supports_reasoning(model_id: str) -> bool:
    model = model_id.lower()
    return (
        "claude-3-7" in model
        or bool(re.search(r"claude-(haiku|sonnet|opus)-4([.\-]|$)", model))
        or bool(re.search(r"claude-opus-4\.\d+", model))
    )


def supports_adaptive_reasoning(model_id: str) -> bool:
    model = model_id.lower()
    return "opus-4-6" in model or "opus-4.6" in model


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


def _map_effort_to_anthropic(effort: str) -> str:
    if effort == "max":
        return "max"
    if effort in {"minimal", "low"}:
        return "low"
    if effort == "medium":
        return "medium"
    return "high"


def _coerce_positive_int(value: Any, *, default: int) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return default
    return parsed if parsed > 0 else default


def _reasoning_effort_from_request(req: dict[str, Any]) -> str | None:
    options = req.get("options")
    if not isinstance(options, dict):
        return None
    for key in ("reasoningEffort", "reasoning_effort"):
        if isinstance(options.get(key), str):
            return _normalize_reasoning_effort(options[key])
    params = options.get("requestParams")
    if isinstance(params, dict):
        for key in ("reasoningEffort", "reasoning_effort"):
            if isinstance(params.get(key), str):
                return _normalize_reasoning_effort(params[key])
    return None


def _thinking_budget_from_request(req: dict[str, Any], effort: str) -> int:
    options = req.get("options")
    if isinstance(options, dict):
        thinking = options.get("thinking")
        if isinstance(thinking, dict):
            budget = thinking.get("budgetTokens")
            if budget is not None:
                return _coerce_positive_int(budget, default=REASONING_BUDGETS["medium"])
    return REASONING_BUDGETS.get(effort, REASONING_BUDGETS["medium"])


def _prepend_system(body: dict[str, Any]) -> None:
    block = {"type": "text", "text": CLAUDE_SYSTEM}
    system = body.get("system")
    if isinstance(system, str):
        body["system"] = [block, {"type": "text", "text": system}]
    elif isinstance(system, list):
        body["system"] = [block, *system]
    else:
        body["system"] = [block]


def _apply_reasoning(req: dict[str, Any]) -> dict[str, Any]:
    model_id = str(req.get("model") or "")
    effort = _reasoning_effort_from_request(req)
    if effort not in {None, "none", "auto"} and supports_reasoning(model_id):
        body = req.get("body")
        if not isinstance(body, dict):
            body = {}
            req["body"] = body
        if supports_adaptive_reasoning(model_id):
            body["thinking"] = {"type": "adaptive"}
            body["output_config"] = {"effort": _map_effort_to_anthropic(effort)}
        else:
            budget = _thinking_budget_from_request(req, effort)
            body["thinking"] = {"type": "enabled", "budget_tokens": budget}

    options = req.get("options")
    if isinstance(options, dict):
        options.pop("reasoningEffort", None)
        options.pop("reasoning_effort", None)
        params = options.get("requestParams")
        if isinstance(params, dict):
            params.pop("reasoningEffort", None)
            params.pop("reasoning_effort", None)
    return req


def _exchange_code(code: str, state: str, verifier: str) -> dict[str, Any]:
    if not code or not verifier:
        raise ValueError("Missing authorization code")
    response = httpx.post(
        TOKEN_URL,
        json={
            "grant_type": "authorization_code",
            "client_id": _decode_client_id(),
            "code": code,
            "state": state or verifier,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
        },
        headers={"Content-Type": "application/json"},
        timeout=20.0,
    )
    if response.status_code != 200:
        raise ValueError(response.text)
    payload = response.json()
    access_token = payload.get("access_token")
    refresh_token = payload.get("refresh_token")
    expires_in = payload.get("expires_in")
    if not access_token or not refresh_token or not isinstance(expires_in, int):
        raise ValueError("Invalid token response")
    return {
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresIn": expires_in,
    }


def _refresh_tokens(refresh_token: str) -> dict[str, Any]:
    if not refresh_token:
        raise ValueError("Missing refresh token")
    response = httpx.post(
        TOKEN_URL,
        json={
            "grant_type": "refresh_token",
            "client_id": _decode_client_id(),
            "refresh_token": refresh_token,
        },
        headers={"Content-Type": "application/json"},
        timeout=20.0,
    )
    if response.status_code != 200:
        raise ValueError(response.text)
    payload = response.json()
    access_token = payload.get("access_token")
    expires_in = payload.get("expires_in")
    if not access_token or not isinstance(expires_in, int):
        raise ValueError("Invalid token response")
    return {
        "accessToken": access_token,
        "refreshToken": payload.get("refresh_token") or refresh_token,
        "expiresIn": expires_in,
    }


class ClaudeOAuth(Provider):
    id = "anthropic_oauth"
    name = "Claude OAuth"

    def transport(self, ctx: ProviderContext, model: str) -> Transport:
        return Transport(
            dialect="anthropic-messages",
            base_url="https://api.anthropic.com",
            headers=ANTHROPIC_HEADERS,
        )

    async def prepare(self, ctx: ProviderContext, req: dict[str, Any]) -> dict[str, Any]:
        req = _apply_reasoning(req)
        body = req.get("body")
        if not isinstance(body, dict):
            body = {}
            req["body"] = body
        _prepend_system(body)
        return req

    async def login(self, ctx: ProviderContext) -> Auth:
        verifier, challenge = _pkce()
        auth_url = f"{AUTHORIZE_URL}?{urlencode({
            'code': 'true',
            'client_id': _decode_client_id(),
            'response_type': 'code',
            'redirect_uri': REDIRECT_URI,
            'scope': SCOPE,
            'code_challenge': challenge,
            'code_challenge_method': 'S256',
            'state': verifier,
        })}"
        await ctx.ui.show(
            elements=[Field.link("Sign in to Claude", auth_url, open_on_start=True)],
            instructions="Paste the authorization code after signing in.",
        )
        code = (await ctx.ui.prompt("Paste the authorization code")).strip()
        tokens = _exchange_code(code, verifier, verifier)
        return Auth(
            access=tokens["accessToken"],
            refresh=tokens["refreshToken"],
            expires_in=tokens["expiresIn"],
        )

    async def refresh(self, ctx: ProviderContext, auth: Auth) -> Auth:
        tokens = _refresh_tokens(str(auth.refresh or ""))
        return Auth(
            access=tokens["accessToken"],
            refresh=tokens["refreshToken"],
            expires_in=tokens["expiresIn"],
            keep_fresh=auth.keep_fresh,
        )


PLUGIN = ClaudeOAuth()
