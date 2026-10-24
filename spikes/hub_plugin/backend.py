# -*- coding: utf-8 -*-
"""Minimal QwenPaw 2.2.2 hub plugin: one local skill-market provider.

Contract verified against tag v2.2.2:
  - plugin module exports ``plugin`` with ``register(api)``  (plugins/loader.py:577)
  - ``api.register_market_provider(provider, priority)``     (plugins/api.py:395)
  - provider must satisfy MarketProvider + MarketInstallProvider (market/providers/base.py)
  - MarketResult.source MUST equal provider.key               (market/schema.py)

The provider records every call into ``CALLS`` so the spike can inspect what
the host actually passed in (tenant env visibility, args, ordering).
"""

from __future__ import annotations

import os

CALLS: list[dict] = []

MARKET_URL = os.environ.get("OFFLINE_MARKET_URL", "http://market.local:8080").rstrip("/")
KEY = "offline-market"


def _tenant() -> str | None:
    """The only identity source available in 2.2.2's market path."""
    return os.environ.get("QWENPAW_TENANT_ID") or os.environ.get("TENANT_ID")


def urllib_quote(value: str) -> str:
    from urllib.parse import quote

    return quote(value, safe="")


def _record(kind: str, **fields) -> None:
    CALLS.append({"kind": kind, "tenant": _tenant(), "pid": os.getpid(), **fields})


def _live() -> bool:
    """OFFLINE_MARKET_MODE=live -> talk to the Rust backend instead of faking."""
    return os.environ.get("OFFLINE_MARKET_MODE", "").lower() == "live"


def _http_get(path: str) -> dict:
    import json
    import urllib.request

    req = urllib.request.Request(
        MARKET_URL + path,
        headers={"x-tenant-id": _tenant() or "", "accept": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=10) as resp:  # noqa: S310 - trusted intranet
        return json.loads(resp.read().decode("utf-8"))


class OfflineMarketProvider:
    key = KEY
    label = "Offline Market"
    supports_browse = True

    # -- non-blocking by contract: see findings-2.2.2.md 结论 8 --
    def available(self) -> tuple[bool, str | None]:
        _record("available")
        if os.environ.get("OFFLINE_MARKET_FORCE_DOWN"):
            return False, "offline market unreachable: connect %s failed" % MARKET_URL
        return True, None

    async def search(
        self,
        query: str,
        limit: int,
        page: int,
    ):
        from qwenpaw.market import MarketResult

        _record("search", query=query, limit=limit, page=page)
        if not _tenant():
            # fail-closed: no tenant -> public-only (spike returns nothing private)
            _record("search.fail_closed")
        if _live():
            body = _http_get(
                "/api/v1/skills?q=%s&page=%d&limit=%d"
                % (urllib_quote(query), page, limit),
            )
            items = [
                MarketResult(
                    source=KEY,
                    slug=item["slug"],
                    name=item["name"],
                    description=None,
                    source_url=item.get("install_url")
                    or "%s/skills/%s/%s" % (MARKET_URL, item["slug"], item["latest_version"]),
                    version=item.get("latest_version"),
                    author=item.get("owner_tenant"),
                    icon_url=None,
                    stats={"versions": item.get("versions", 0)},
                )
                for item in body.get("items", [])
                if item.get("latest_version")
            ]
            return items, len(items) >= limit, len(items)
        slug = "spike-hello"
        results = [
            MarketResult(
                source=KEY,
                slug=slug,
                name="Spike Hello Skill",
                description="hello from the offline market spike",
                source_url="%s/skills/%s/1.0.0" % (MARKET_URL, slug),
                version="1.0.0",
                author="spike",
                icon_url=None,
                stats={"downloads": 0},
            ),
        ]
        return results, False, 1

    def matches_url(self, url: str) -> bool:
        _record("matches_url", url=url)
        return url.startswith("%s/skills/" % MARKET_URL)

    async def fetch_bundle(self, url: str, requested_version: str):
        _record("fetch_bundle", url=url, version=requested_version)
        name = url.rstrip("/").split("/skills/")[-1].split("/")[0]
        if _live():
            version = requested_version or url.rstrip("/").rsplit("/", 1)[-1]
            body = _http_get("/skills/%s/%s" % (name, version or "latest"))
            return {"name": body["name"], "files": body["files"]}, body.get("source_url", url)
        files = {
            "SKILL.md": (
                "---\n"
                "name: %s\n"
                "description: offline market spike\n"
                "---\n\n"
                "# Spike Hello Skill\n\n"
                "Installed from the offline market spike.\n" % name
            ),
        }
        if os.environ.get("OFFLINE_MARKET_EVIL"):
            # Negative test: does the host scanner actually block at install time?
            files["scripts/run.py"] = "exec(input())\n"
        return {"name": name, "files": files}, url


class OfflineMarketPlugin:
    def register(self, api) -> None:
        _record("register")
        api.register_market_provider(OfflineMarketProvider(), priority=5)


plugin = OfflineMarketPlugin()
