from __future__ import annotations

import base64
import hashlib
import secrets
import uuid
from typing import Any
from urllib.parse import urlencode

import httpx
from covalt.provider import Auth, Control, Model, Pricing, Provider, Tag, Transport
from covalt.provider.context import ProviderContext

XAI_API_BASE = "https://api.x.ai/v1"
CLI_PROXY_BASE = "https://cli-chat-proxy.grok.com/v1"
XAI_MODELS_URL = f"{XAI_API_BASE}/models"
CLI_MODELS_URL = f"{CLI_PROXY_BASE}/models"

OAUTH_ISSUER = "https://auth.x.ai"
OAUTH_DISCOVERY_URL = f"{OAUTH_ISSUER}/.well-known/openid-configuration"
OAUTH_CLIENT_ID = "b1a00492-073a-47ea-816f-4c329264a828"
OAUTH_SCOPE = "openid profile email offline_access grok-cli:access api:access"
OAUTH_REFRESH_SKEW_SEC = 120
GROK_CLI_VERSION = "0.2.16"

CLI_PROXY_MODELS = frozenset({"grok-build", "grok-composer-2.5-fast"})
REASONING_MODEL_PREFIXES = ("grok-3-mini", "grok-4.20-multi-agent", "grok-4.3")


def _pkce() -> tuple[str, str]:
    verifier = base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode()
    challenge = base64.urlsafe_b64encode(
        hashlib.sha256(verifier.encode()).digest()
    ).rstrip(b"=").decode()
    return verifier, challenge


def _access_token(ctx: ProviderContext) -> str | None:
    if ctx.auth.access_token:
        return ctx.auth.access_token.strip() or None
    if ctx.auth.api_key:
        return ctx.auth.api_key.strip() or None
    return None


def _validate_xai_url(url: str) -> str:
    if not url.startswith("https://"):
        raise ValueError(f"unexpected xAI OAuth endpoint: {url}")
    host = url.split("/")[2].lower()
    if host != "x.ai" and not host.endswith(".x.ai"):
        raise ValueError(f"unexpected xAI OAuth endpoint: {url}")
    return url


async def _oauth_discovery() -> dict[str, str]:
    async with httpx.AsyncClient(timeout=20.0) as client:
        resp = await client.get(OAUTH_DISCOVERY_URL, headers={"Accept": "application/json"})
    if resp.status_code != 200:
        raise ValueError(f"xAI OAuth discovery failed: {resp.status_code} {resp.text}")
    data = resp.json()
    auth_endpoint = data.get("authorization_endpoint")
    token_endpoint = data.get("token_endpoint")
    if not auth_endpoint or not token_endpoint:
        raise ValueError("xAI OAuth discovery missing authorization/token endpoints")
    return {
        "authorization_endpoint": _validate_xai_url(str(auth_endpoint)),
        "token_endpoint": _validate_xai_url(str(token_endpoint)),
    }


async def _exchange_token(token_endpoint: str, body: dict[str, str]) -> dict[str, Any]:
    async with httpx.AsyncClient(timeout=20.0) as client:
        resp = await client.post(
            token_endpoint,
            data=body,
            headers={
                "Accept": "application/json",
                "Content-Type": "application/x-www-form-urlencoded",
            },
        )
    if resp.status_code != 200:
        raise ValueError(f"xAI token request failed: {resp.status_code} {resp.text}")
    payload = resp.json()
    access = payload.get("access_token")
    refresh = payload.get("refresh_token")
    expires_in = payload.get("expires_in")
    if not access or not refresh or not isinstance(expires_in, int):
        raise ValueError("xAI token response missing access_token, refresh_token, or expires_in")
    return payload


def _auth_from_token(payload: dict[str, Any], token_endpoint: str) -> Auth:
    expires_in = int(payload["expires_in"]) - OAUTH_REFRESH_SKEW_SEC
    if expires_in < 1:
        expires_in = 1
    return Auth(
        access=str(payload["access_token"]),
        refresh=str(payload["refresh_token"]),
        expires_in=expires_in,
        extra={"tokenEndpoint": token_endpoint},
    )


def _price_per_million(value: Any) -> float | None:
    if not isinstance(value, (int, float)):
        return None
    return float(value) / 10_000.0


def _model_kind_tag(model_id: str) -> Tag | None:
    lowered = model_id.lower()
    if "imagine-image" in lowered:
        return Tag("Image", tone="neutral")
    if "imagine-video" in lowered:
        return Tag("Video", tone="neutral")
    if lowered in CLI_PROXY_MODELS:
        return Tag("Coding", tone="positive")
    return None


def _supports_reasoning(model_id: str) -> bool:
    normalized = model_id.lower().split("/")[-1]
    return any(normalized.startswith(prefix) for prefix in REASONING_MODEL_PREFIXES)


def _model_controls(model_id: str) -> list[Control]:
    if not _supports_reasoning(model_id):
        return []
    return [
        Control.segmented(
            "reasoning",
            ["none", "low", "medium", "high"],
            default="medium",
            label="Reasoning",
        )
    ]


def _model_from_api_entry(entry: dict[str, Any]) -> Model | None:
    model_id = entry.get("id")
    if not isinstance(model_id, str) or not model_id.strip():
        return None
    model_id = model_id.strip()
    aliases = entry.get("aliases")
    name = model_id
    if isinstance(aliases, list) and aliases:
        first = aliases[0]
        if isinstance(first, str) and first.strip():
            name = first.strip()

    context = entry.get("context_length") or entry.get("context_window")
    context_window = int(context) if isinstance(context, (int, float)) else None

    pricing = None
    input_price = _price_per_million(entry.get("prompt_text_token_price"))
    output_price = _price_per_million(entry.get("completion_text_token_price"))
    cached_price = _price_per_million(entry.get("cached_prompt_text_token_price"))
    if input_price is not None or output_price is not None or cached_price is not None:
        pricing = Pricing(input=input_price, output=output_price, cached_input=cached_price, per="1M")

    tags: list[Tag] = []
    kind_tag = _model_kind_tag(model_id)
    if kind_tag:
        tags.append(kind_tag)

    return Model(
        id=model_id,
        name=name,
        description=f"xAI model ({model_id})",
        context_window=context_window,
        pricing=pricing,
        tags=tags,
        details={"Provider": "xAI"},
        controls=_model_controls(model_id),
    )


def _model_from_cli_entry(entry: dict[str, Any]) -> Model | None:
    model_id = entry.get("id") or entry.get("model")
    if not isinstance(model_id, str) or not model_id.strip():
        return None
    model_id = model_id.strip()
    name = entry.get("name")
    if not isinstance(name, str) or not name.strip():
        name = model_id
    description = entry.get("description")
    if not isinstance(description, str) or not description.strip():
        description = f"xAI coding model ({model_id})"

    context = entry.get("context_window")
    context_window = int(context) if isinstance(context, (int, float)) else None

    tags: list[Tag] = [Tag("Coding", tone="positive")]
    if entry.get("supports_backend_search"):
        tags.append(Tag("Search", tone="positive"))

    details: dict[str, str] = {"Provider": "xAI"}
    agent_type = entry.get("agent_type")
    if isinstance(agent_type, str) and agent_type.strip():
        details["Agent type"] = agent_type.strip()

    return Model(
        id=model_id,
        name=name.strip(),
        description=description.strip(),
        context_window=context_window,
        tags=tags,
        details=details,
        controls=_model_controls(model_id),
    )


async def _fetch_models(url: str, token: str) -> list[dict[str, Any]]:
    async with httpx.AsyncClient(timeout=20.0) as client:
        resp = await client.get(
            url,
            headers={
                "Authorization": f"Bearer {token}",
                "Accept": "application/json",
            },
        )
    if resp.status_code != 200:
        raise ValueError(f"models fetch failed ({url}): {resp.status_code} {resp.text[:400]}")
    payload = resp.json()
    data = payload.get("data")
    if not isinstance(data, list):
        return []
    return [item for item in data if isinstance(item, dict)]


def _reasoning_effort(req: dict[str, Any]) -> str | None:
    options = req.get("options")
    if not isinstance(options, dict):
        return None
    for key in ("reasoning", "reasoningEffort", "reasoning_effort"):
        value = options.get(key)
        if isinstance(value, str) and value.strip():
            normalized = value.strip().lower()
            if normalized in {"none", "off"}:
                return "none"
            if normalized in {"low", "medium", "high"}:
                return normalized
    params = options.get("requestParams")
    if isinstance(params, dict):
        for key in ("reasoning", "reasoningEffort", "reasoning_effort"):
            value = params.get(key)
            if isinstance(value, str) and value.strip():
                normalized = value.strip().lower()
                if normalized in {"none", "off", "low", "medium", "high"}:
                    return "none" if normalized in {"none", "off"} else normalized
    return None


def _is_cli_proxy_model(model: str) -> bool:
    return model.strip().lower() in CLI_PROXY_MODELS


def _cli_proxy_headers(model: str) -> dict[str, str]:
    return {
        "x-grok-client-identifier": "covalt-xai-oauth",
        "x-grok-client-version": GROK_CLI_VERSION,
        "x-xai-token-auth": "xai-grok-cli",
        "x-grok-model-override": model,
    }


class XaiOAuth(Provider):
    id = "xai_oauth"
    name = "xAI (Grok)"

    def transport(self, ctx: ProviderContext, model: str) -> Transport:
        if _is_cli_proxy_model(model):
            return Transport(
                dialect="openai-responses",
                base_url=CLI_PROXY_BASE,
                headers=_cli_proxy_headers(model),
            )
        return Transport(
            dialect="openai-responses",
            base_url=XAI_API_BASE,
            headers={"Accept": "application/json"},
        )

    async def prepare(self, ctx: ProviderContext, req: dict[str, Any]) -> dict[str, Any]:
        model = str(req.get("model") or ctx.model or "")
        effort = _reasoning_effort(req)
        if effort in {None, "none"} or not _supports_reasoning(model):
            return req

        body = dict(req.get("body") or {})
        body["reasoning_effort"] = effort
        return {**req, "body": body}

    async def models(self, ctx: ProviderContext):
        token = _access_token(ctx)
        if not token:
            return

        merged: dict[str, Model] = {}
        try:
            for entry in await _fetch_models(XAI_MODELS_URL, token):
                model = _model_from_api_entry(entry)
                if model:
                    merged[model.id] = model
        except Exception:
            pass

        try:
            for entry in await _fetch_models(CLI_MODELS_URL, token):
                model = _model_from_cli_entry(entry)
                if model:
                    merged[model.id] = model
        except Exception:
            pass

        for model in sorted(merged.values(), key=lambda item: item.name.lower()):
            yield model

    async def login(self, ctx: ProviderContext) -> Auth:
        discovery = await _oauth_discovery()
        verifier, challenge = _pkce()
        state = uuid.uuid4().hex
        redirect_uri = ctx.callback_uri
        if not redirect_uri:
            port = ctx.callback_port or 56121
            redirect_uri = f"http://127.0.0.1:{port}/callback"

        params = {
            "response_type": "code",
            "client_id": OAUTH_CLIENT_ID,
            "redirect_uri": redirect_uri,
            "scope": OAUTH_SCOPE,
            "code_challenge": challenge,
            "code_challenge_method": "S256",
            "state": state,
            "nonce": uuid.uuid4().hex,
        }
        auth_url = f"{discovery['authorization_endpoint']}?{urlencode(params)}"
        await ctx.ui.show(note="Sign in with your xAI / Grok account", url=auth_url)
        callback = await ctx.ui.browser(auth_url)

        code = str(callback.get("code") or "").strip()
        callback_state = str(callback.get("state") or "").strip()
        error = callback.get("error")
        if error:
            description = callback.get("error_description") or error
            raise ValueError(f"xAI authorization failed: {description}")
        if not code:
            raise ValueError("xAI authorization failed: no authorization code returned")
        if callback_state and callback_state != state:
            raise ValueError("xAI authorization failed: state mismatch")

        payload = await _exchange_token(
            discovery["token_endpoint"],
            {
                "grant_type": "authorization_code",
                "code": code,
                "redirect_uri": redirect_uri,
                "client_id": OAUTH_CLIENT_ID,
                "code_verifier": verifier,
            },
        )
        return _auth_from_token(payload, discovery["token_endpoint"])

    async def refresh(self, ctx: ProviderContext, auth: Auth) -> Auth:
        refresh_token = (auth.refresh or "").strip()
        if not refresh_token:
            raise ValueError("Missing refresh token")

        extra = auth.extra if isinstance(auth.extra, dict) else {}
        token_endpoint = extra.get("tokenEndpoint") or extra.get("token_endpoint")
        if not isinstance(token_endpoint, str) or not token_endpoint.strip():
            discovery = await _oauth_discovery()
            token_endpoint = discovery["token_endpoint"]
        else:
            token_endpoint = _validate_xai_url(token_endpoint.strip())

        payload = await _exchange_token(
            token_endpoint,
            {
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": OAUTH_CLIENT_ID,
            },
        )
        return _auth_from_token(payload, token_endpoint)


PLUGIN = XaiOAuth()