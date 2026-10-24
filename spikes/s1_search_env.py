# -*- coding: utf-8 -*-
"""Spike 1: does the market path see the tenant, and does the contract hold?

Runs against an installed qwenpaw==2.2.2 (Windows/Linux), no QwenPaw server needed.

Proves:
  A. our plugin's register(api) is accepted by the real market registry
  B. search_market() reaches the provider in-process (same PID) -> env readable
  C. QWENPAW_TENANT_ID is visible from inside search()/fetch_bundle()
  D. duplicate key is rejected loudly
  E. unload while a request is in flight raises MarketProviderBusyError

Run:  .venv/Scripts/python.exe s1_search_env.py
"""

from __future__ import annotations

import asyncio
import importlib.util
import json
import os
import sys
from pathlib import Path

os.environ["QWENPAW_TENANT_ID"] = "tenant-spike-42"
os.environ.setdefault("OFFLINE_MARKET_URL", "http://market.local:8080")

HERE = Path(__file__).resolve().parent
PLUGIN = HERE / "hub_plugin" / "backend.py"


def load_plugin_module():
    spec = importlib.util.spec_from_file_location("spike_hub_plugin", PLUGIN)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["spike_hub_plugin"] = mod
    spec.loader.exec_module(mod)
    return mod


class FakeApi:
    """Mimics PluginApi.register_market_provider (plugins/api.py:395)."""

    def __init__(self, plugin_id: str):
        self.plugin_id = plugin_id

    def register_market_provider(self, provider, priority: int = 100) -> None:
        from qwenpaw.market import market_registry

        market_registry.register(self.plugin_id, provider, priority)


def main() -> int:
    from qwenpaw.market import market_registry, search_market
    from qwenpaw.market.registry import MarketProviderBusyError

    mod = load_plugin_module()
    report: dict = {"checks": {}}

    # A: register through our plugin's own register(api)
    mod.plugin.register(FakeApi("offline-market-spike"))
    keys = list(market_registry.snapshot())
    report["checks"]["A_registered"] = {
        "keys": keys,
        "ok": mod.KEY in keys,
    }

    # B + C: run the real search pipeline
    results, errors, by_provider = asyncio.run(
        search_market(query="hello", provider_pages={mod.KEY: 1}, limit=5),
    )
    search_calls = [c for c in mod.CALLS if c["kind"] == "search"]
    report["checks"]["B_search_reached"] = {
        "results": [r.slug for r in results],
        "errors": [e.message for e in errors],
        "by_provider": by_provider,
        "ok": len(results) == 1 and results[0].source == mod.KEY and not errors,
    }
    report["checks"]["C_tenant_visible"] = {
        "pid_host": os.getpid(),
        "pid_provider": search_calls[0]["pid"] if search_calls else None,
        "tenant_seen": search_calls[0]["tenant"] if search_calls else None,
        "same_process": bool(search_calls) and search_calls[0]["pid"] == os.getpid(),
        "ok": bool(search_calls) and search_calls[0]["tenant"] == "tenant-spike-42",
    }

    # fetch_bundle via the install-side contract
    provider = market_registry.snapshot()[mod.KEY]
    url = "%s/skills/spike-hello/1.0.0" % mod.MARKET_URL
    matched = provider.matches_url(url)
    bundle, resolved = asyncio.run(provider.fetch_bundle(url, "1.0.0"))
    report["checks"]["C2_fetch_contract"] = {
        "matches_url": matched,
        "name": bundle.get("name"),
        "files": sorted(bundle.get("files", {})),
        "resolved_url": resolved,
        "ok": matched and "SKILL.md" in bundle.get("files", {}),
    }

    # D: duplicate key must fail loudly
    try:
        market_registry.register("another-owner", provider, 1)
        dup = {"raised": None, "ok": False}
    except ValueError as exc:
        dup = {"raised": "ValueError", "message": str(exc), "ok": True}
    report["checks"]["D_duplicate_key"] = dup

    # E: unload while in flight -> busy
    busy = {"raised": None, "ok": False}
    with market_registry.use(mod.KEY):
        try:
            market_registry.begin_unload("offline-market-spike")
        except MarketProviderBusyError as exc:
            busy = {"raised": "MarketProviderBusyError", "message": str(exc), "ok": True}
    report["checks"]["E_busy_guard"] = busy

    report["calls"] = mod.CALLS
    report["all_ok"] = all(v["ok"] for v in report["checks"].values())
    print(json.dumps(report, indent=2, ensure_ascii=False))
    return 0 if report["all_ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
