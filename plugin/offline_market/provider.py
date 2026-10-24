# -*- coding: utf-8 -*-
"""Market provider + install provider for the offline market.

Contract (QwenPaw 2.2.2, `qwenpaw/market/providers/base.py`):

    MarketProvider:        key, label, supports_browse, available(), search(q, limit, page)
    MarketInstallProvider: matches_url(url), fetch_bundle(url, requested_version)

Both are duck-typed Protocols; `MarketRegistry.register` rejects a plugin that
does not satisfy both. Upload, validation, scanning and installation stay in
the host — this provider only answers "what exists" and "give me the files".
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass

from .client import BackendClient, BackendError
from .runtime import PluginSettings

KEY = "offline-market"
LABEL = "离线技能市场"

# How long a failed request keeps the market reported as unavailable.
_HEALTH_TTL = 60.0


@dataclass
class _Health:
    ok: bool = True
    reason: str | None = None
    at: float = 0.0

    def note_ok(self) -> None:
        self.ok, self.reason, self.at = True, None, time.monotonic()

    def note_error(self, message: str) -> None:
        self.ok, self.reason, self.at = False, message, time.monotonic()

    def stale_reason(self) -> str | None:
        if self.ok or not self.reason:
            return None
        if time.monotonic() - self.at > _HEALTH_TTL:
            return None
        return self.reason


class OfflineMarketProvider:
    key = KEY
    label = LABEL
    supports_browse = True

    def __init__(self, settings: PluginSettings) -> None:
        self.settings = settings
        self._health = _Health()

    # ------------------------------------------------------------- wiring ---

    def _client(self) -> BackendClient:
        url = self.settings.market_url
        tenant = self.settings.tenant
        if not url or not tenant:
            raise BackendError(
                0,
                "not_configured",
                "离线技能市场未配置：需要 OFFLINE_MARKET_URL 与 QWENPAW_TENANT_ID",
            )
        return BackendClient(url, tenant, timeout=self.settings.timeout)

    def _note_failure(self, exc: BackendError) -> None:
        """Only the market being *down* counts against availability.

        403/410 are business answers (private skill, taken down): the market is
        perfectly healthy, and treating them as an outage would blank the whole
        market for the next minute after any takedown (found by preflight).
        """
        if exc.status == 0 or exc.status >= 500:
            self._health.note_error(str(exc))

    # ------------------------------------------------------------ contract ---

    def available(self) -> tuple[bool, str | None]:
        """Non-blocking by contract: no network, only local checks + cache."""
        problems = self.settings.problems()
        if problems:
            return False, "；".join(problems)
        stale = self._health.stale_reason()
        if stale:
            return False, stale
        return True, None

    async def search(
        self,
        query: str,
        limit: int,
        page: int,
    ):
        from qwenpaw.market import MarketResult

        client = self._client()
        try:
            body = await asyncio.to_thread(
                client.list_skills, query, page, min(int(limit or 20), 50)
            )
        except BackendError as exc:
            self._note_failure(exc)
            raise
        self._health.note_ok()

        items = body.get("items", []) if isinstance(body, dict) else []
        results = [
            MarketResult(
                source=KEY,
                slug=item["slug"],
                name=item.get("name") or item["slug"],
                description=None,
                source_url=item.get("install_url")
                or f"{client.base_url}/skills/{item['slug']}/{item.get('latest_version') or 'latest'}",
                version=item.get("latest_version"),
                author=item.get("owner_tenant"),
                icon_url=None,
                stats={
                    "versions": item.get("versions", 0),
                    "visibility": item.get("visibility", "public"),
                },
            )
            for item in items
            if item.get("latest_version")
        ]
        has_more = len(results) >= int(limit or 20)
        return results, has_more, len(results)

    def matches_url(self, url: str) -> bool:
        base = self.settings.market_url
        if not base or not url:
            return False
        return url.startswith(f"{base}/skills/")

    async def fetch_bundle(self, url: str, requested_version: str):
        slug, version = _split_bundle_url(url)
        if requested_version:
            version = requested_version
        client = self._client()
        try:
            body = await asyncio.to_thread(client.fetch_bundle, slug, version or "latest")
        except BackendError as exc:
            self._note_failure(exc)
            raise
        self._health.note_ok()

        if not isinstance(body, dict) or not isinstance(body.get("files"), dict):
            raise BackendError(0, "bad_bundle", "市场后端返回的 bundle 结构不合法")
        bundle = {"name": body.get("name") or slug, "files": body["files"]}
        return bundle, body.get("source_url") or url


def _split_bundle_url(url: str) -> tuple[str, str]:
    """`http://host/skills/<slug>/<version>` -> (slug, version)."""
    tail = url.split("/skills/", 1)[-1].strip("/")
    parts = [part for part in tail.split("/") if part]
    if not parts:
        raise BackendError(0, "bad_url", f"无法从 URL 解析技能标识：{url}")
    slug = parts[0]
    version = parts[1] if len(parts) > 1 else "latest"
    return slug, version
