# -*- coding: utf-8 -*-
"""Test doubles: a fake market backend transport, no network, no host needed."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

PLUGIN_ROOT = Path(__file__).resolve().parents[1]
if str(PLUGIN_ROOT) not in sys.path:
    sys.path.insert(0, str(PLUGIN_ROOT))


class FakeBackend:
    """Records requests and answers from a routing table."""

    def __init__(self) -> None:
        self.requests: list[dict] = []
        self.routes: dict[tuple[str, str], tuple[int, object]] = {}

    def on(self, method: str, path: str, status: int, body: object) -> "FakeBackend":
        self.routes[(method.upper(), path)] = (status, body)
        return self

    def __call__(self, method, url, headers, body, timeout):
        path = url.split("://", 1)[-1]
        path = "/" + path.split("/", 1)[1] if "/" in path else "/"
        route_key = (method.upper(), path.split("?", 1)[0])
        self.requests.append(
            {
                "method": method.upper(),
                "url": url,
                "path": path,
                "headers": dict(headers),
                "body": json.loads(body) if body else None,
                "timeout": timeout,
            }
        )
        status, payload = self.routes.get(route_key, (404, {"error": {"code": "not_found", "message": "no route"}}))
        if isinstance(payload, Exception):
            raise payload
        return status, json.dumps(payload).encode("utf-8")


@pytest.fixture
def backend() -> FakeBackend:
    return FakeBackend()


@pytest.fixture
def env(monkeypatch, tmp_path):
    """A runtime that looks like a healthy hub-managed tenant."""
    monkeypatch.setenv("OFFLINE_MARKET_URL", "http://market.test/")
    monkeypatch.setenv("QWENPAW_TENANT_ID", "tenant-a")
    monkeypatch.setenv("QWENPAW_SKILL_SCAN_MODE", "block")
    monkeypatch.setenv("QWENPAW_CONFIG_PATH", str(tmp_path / "config.json"))
    monkeypatch.delenv("OFFLINE_MARKET_TENANT", raising=False)
    monkeypatch.delenv("TENANT_ID", raising=False)
    return monkeypatch
