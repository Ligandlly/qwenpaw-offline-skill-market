# -*- coding: utf-8 -*-
"""Plugin entry point. QwenPaw 2.2.2 requires a module-level `plugin` object
with a `register(api)` method (src/qwenpaw/plugins/loader.py:577-625)."""

from __future__ import annotations

from offline_market.provider import OfflineMarketProvider
from offline_market.runtime import PluginSettings


class OfflineMarketPlugin:
    def register(self, api) -> None:
        settings = PluginSettings(dict(getattr(api, "config", {}) or {}))
        api.register_market_provider(OfflineMarketProvider(settings), priority=5)

        # Our own page (M3) speaks to these routes; the browser never sees the
        # market backend or the tenant id.
        from offline_market.routes import build_router

        api.register_http_router(
            build_router(settings),
            prefix="/offline-market",
            tags=["offline-market"],
        )


plugin = OfflineMarketPlugin()
