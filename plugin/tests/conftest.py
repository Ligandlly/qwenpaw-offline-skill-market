# -*- coding: utf-8 -*-
"""Test doubles: a fake market backend transport, no network, no host needed."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

PLUGIN_ROOT = Path(__file__).resolve().parents[1]
if str(PLUGIN_ROOT) not in sys.path:
    sys.path.insert(0, str(PLUGIN_ROOT))


def _install_qwenpaw_stub() -> None:
    """Stand in for the host's `qwenpaw.market` when QwenPaw is not installed.

    The provider builds `MarketResult` objects for the host, so the type has to
    exist. CI deliberately does not install QwenPaw (40 MB + 80 deps), so the
    tests supply a stub with the 2.2.2 field set. Set OFFLINE_MARKET_STUB_MARKET=1
    to force the stub even where a real QwenPaw is importable.
    """
    if os.environ.get("OFFLINE_MARKET_STUB_MARKET") != "1":
        try:
            import qwenpaw.market  # noqa: F401

            return
        except Exception:  # noqa: BLE001 - any import failure means "not usable"
            pass

    import types
    from dataclasses import dataclass

    @dataclass(frozen=True)
    class MarketResult:  # mirrors qwenpaw/market/schema.py @ v2.2.2
        source: str
        slug: str
        name: str
        description: str | None = None
        source_url: str = ""
        version: str | None = None
        author: str | None = None
        icon_url: str | None = None
        stats: dict | None = None

    package = types.ModuleType("qwenpaw")
    market = types.ModuleType("qwenpaw.market")
    market.MarketResult = MarketResult
    package.market = market
    sys.modules["qwenpaw"] = package
    sys.modules["qwenpaw.market"] = market


_install_qwenpaw_stub()


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
    # The hardcoded entry is a deployment switch (default empty). Keep tests
    # independent of whatever address an operator baked into their checkout.
    monkeypatch.setattr("offline_market.runtime.HARDCODED_MARKET_URL", "")
    monkeypatch.setenv("QWENPAW_TENANT_ID", "tenant-a")
    monkeypatch.setenv("QWENPAW_SKILL_SCAN_MODE", "block")
    monkeypatch.setenv("QWENPAW_CONFIG_PATH", str(tmp_path / "config.json"))
    monkeypatch.setenv("QWENPAW_WORKING_DIR", str(tmp_path / "home"))
    monkeypatch.delenv("OFFLINE_MARKET_TENANT", raising=False)
    monkeypatch.delenv("TENANT_ID", raising=False)
    return monkeypatch
