from __future__ import annotations

from typing import Any


def capabilities() -> dict[str, bool]:
    return {"oauth": False, "prepare": True, "stream": False}


def prepare(req: dict[str, Any]) -> dict[str, Any]:
    return req
