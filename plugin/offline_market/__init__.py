# -*- coding: utf-8 -*-
"""Offline market plugin for QwenPaw 2.2.2.

Surfaces:

1. a ``MarketProvider`` + ``MarketInstallProvider`` pair registered through
   ``PluginApi.register_market_provider`` so the host's own Skills page can
   search and install from this market;
2. plugin HTTP routes under ``/api/offline-market`` used by our own sidebar
   page: skill publish/mine/withdraw/status **and** the agent market
   (snapshot publish, install, uninstall, installed ledger);
3. (M3) the frontend bundle that renders that page.

Identity: the tenant is whatever ``QWENPAW_TENANT_ID`` says in this runtime.
Nothing else is trusted, and nothing is invented when it is missing.
"""

from __future__ import annotations

from .client import BackendClient, BackendError
from .provider import OfflineMarketProvider
from .runtime import PluginSettings

__all__ = [
    "BackendClient",
    "BackendError",
    "OfflineMarketProvider",
    "PluginSettings",
]

KEY = "offline-market"
