from __future__ import annotations

import re
from typing import Any

REASONING_BUDGETS = {
    "minimal": 1024,
    "low": 2048,
    "medium": 8192,
    "high": 16384,
    "max": 32000,
    "xhigh": 32000,
}


def capabilities() -> dict[str, bool]:
    return {"oauth": False, "prepare": True, "stream": False}


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
    if isinstance(options, dict):
        effort = options.get("reasoningEffort")
        if isinstance(effort, str):
            return _normalize_reasoning_effort(effort)
        params = options.get("requestParams")
        if isinstance(params, dict):
            carrier = params.get("reasoningEffort")
            if isinstance(carrier, str):
                return _normalize_reasoning_effort(carrier)
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


def prepare(req: dict[str, Any]) -> dict[str, Any]:
    model_id = str(req.get("model") or "")
    effort = _reasoning_effort_from_request(req)
    if effort in {None, "none", "auto"} or not supports_reasoning(model_id):
        return req

    body = req.get("body")
    if not isinstance(body, dict):
        body = {}
        req["body"] = body

    if supports_adaptive_reasoning(model_id):
        body["thinking"] = {"type": "adaptive"}
        body["output_config"] = {"effort": _map_effort_to_anthropic(effort)}
        return req

    budget = _thinking_budget_from_request(req, effort)
    body["thinking"] = {"type": "enabled", "budget_tokens": budget}
    return req
