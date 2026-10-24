# -*- coding: utf-8 -*-
"""Spike 4: when the market backend is down, what does the user actually see?

Proves:
  A. list_providers() surfaces available()/reason verbatim (-> GET /api/market/providers)
  B. a down provider turns into a MarketSearchError whose message IS the reason
     (market/service.py:_search_provider), i.e. our wording is the user-facing text
  C. available() must be cheap: it is called on every provider listing

Run:  .venv/Scripts/python.exe s4_available_reason.py
"""

from __future__ import annotations

import asyncio
import json
import os
import time

os.environ["QWENPAW_TENANT_ID"] = "tenant-spike-42"
os.environ["OFFLINE_MARKET_FORCE_DOWN"] = "1"

REASON = "offline market unreachable: connect http://market.local:8080 failed"


class DownProvider:
    key = "offline-market-down"
    label = "Offline Market (down)"
    supports_browse = True

    def __init__(self) -> None:
        self.calls = 0

    def available(self):
        self.calls += 1
        return False, REASON

    async def search(self, query, limit, page):
        raise AssertionError("search must not run when available() is False")

    def matches_url(self, url):
        return True

    async def fetch_bundle(self, url, requested_version):
        raise AssertionError("fetch must not run when available() is False")


def main() -> int:
    from qwenpaw.market import list_providers, market_registry, search_market

    provider = DownProvider()
    market_registry.register("spike-down", provider, 1)

    t0 = time.perf_counter()
    infos = [p for p in list_providers() if p.key == provider.key]
    listing_ms = (time.perf_counter() - t0) * 1000

    results, errors, _ = asyncio.run(
        search_market(query="x", provider_pages={provider.key: 1}, limit=5),
    )

    report = {
        "checks": {
            "A_listing_reason": {
                "available": infos[0].available if infos else None,
                "reason": infos[0].reason if infos else None,
                "ok": bool(infos) and infos[0].reason == REASON,
            },
            "B_error_message_is_reason": {
                "errors": [e.message for e in errors],
                "results": [r.slug for r in results],
                "ok": len(errors) == 1 and errors[0].message == REASON and not results,
            },
            "C_available_called_once_per_listing": {
                "calls": provider.calls,
                "listing_ms": round(listing_ms, 2),
                "ok": provider.calls >= 1,
            },
        },
    }
    report["all_ok"] = all(v["ok"] for v in report["checks"].values())
    print(json.dumps(report, indent=2, ensure_ascii=False))
    return 0 if report["all_ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
