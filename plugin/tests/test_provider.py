# -*- coding: utf-8 -*-
"""Provider behaviour against the host's 2.2.2 contract."""

from __future__ import annotations

import asyncio

import pytest

from offline_market.client import BackendClient, BackendError
from offline_market.provider import OfflineMarketProvider
from offline_market.runtime import PluginSettings


def provider(backend=None, **config) -> OfflineMarketProvider:
    settings = PluginSettings(dict(config))
    prov = OfflineMarketProvider(settings)
    if backend is not None:
        # inject the fake transport: same seam the real client uses
        prov._client = lambda: BackendClient(  # type: ignore[method-assign]
            settings.market_url, settings.tenant, transport=backend
        )
    return prov


def test_available_when_everything_is_configured(env):
    prov = provider()  # available() needs no transport at all
    assert prov.available() == (True, None)


def test_available_false_when_scan_mode_is_off(env, tmp_path):
    env.delenv("QWENPAW_SKILL_SCAN_MODE")
    cfg = tmp_path / "config.json"
    cfg.write_text('{"security":{"skill_scanner":{"mode":"off"}}}', encoding="utf-8")
    prov = OfflineMarketProvider(PluginSettings(config_path=cfg))
    ok, reason = prov.available()
    assert ok is False and "off" in reason


def test_available_when_scan_mode_is_warn(env, tmp_path):
    env.delenv("QWENPAW_SKILL_SCAN_MODE")
    cfg = tmp_path / "config.json"
    cfg.write_text('{"security":{"skill_scanner":{"mode":"warn"}}}', encoding="utf-8")
    prov = OfflineMarketProvider(PluginSettings(config_path=cfg))
    assert prov.available() == (True, None)


def test_available_false_without_tenant(env):
    env.delenv("QWENPAW_TENANT_ID")
    ok, reason = OfflineMarketProvider(PluginSettings()).available()
    assert ok is False and "QWENPAW_TENANT_ID" in reason


def test_search_maps_backend_rows(env, backend):
    backend.on(
        "GET",
        "/api/v1/skills",
        200,
        {
            "items": [
                {
                    "slug": "demo",
                    "name": "Demo Skill",
                    "latest_version": "1.0.0",
                    "owner_tenant": "tenant-a",
                    "versions": 3,
                    "visibility": "public",
                    "install_url": "http://market.test/skills/demo/1.0.0",
                }
            ]
        },
    )
    prov = provider(backend)
    results, has_more, total = asyncio.run(prov.search("de", 10, 1))
    assert [r.slug for r in results] == ["demo"]
    assert results[0].source == "offline-market"
    assert results[0].version == "1.0.0"
    assert total == 1 and has_more is False
    # the tenant travels as a header, never as a query parameter
    assert backend.requests[0]["headers"]["x-tenant-id"] == "tenant-a"


def test_search_prefers_the_reported_publisher_name(env, backend):
    backend.on(
        "GET",
        "/api/v1/skills",
        200,
        {
            "items": [
                {
                    "slug": "named",
                    "name": "Named Skill",
                    "latest_version": "1.0.0",
                    "owner_tenant": "personal-179b9d",
                    "owner_name": "admin",
                    "install_url": "http://market.test/skills/named/1.0.0",
                },
                # published before anyone reported a name
                {
                    "slug": "old",
                    "name": "Old Skill",
                    "latest_version": "1.0.0",
                    "owner_tenant": "personal-179b9d",
                    "owner_name": None,
                    "install_url": "http://market.test/skills/old/1.0.0",
                },
            ]
        },
    )
    results, _has_more, total = asyncio.run(provider(backend).search("", 10, 1))
    assert total == 2
    assert [r.author for r in results] == ["admin", "personal-179b9d"]


def test_search_reports_failure_and_then_reports_itself_unavailable(env, backend):
    backend.on("GET", "/api/v1/skills", 500, {"error": {"code": "boom", "message": "backend on fire"}})
    prov = provider(backend)
    with pytest.raises(BackendError) as excinfo:
        asyncio.run(prov.search("", 10, 1))
    assert "backend on fire" in str(excinfo.value)
    ok, reason = prov.available()
    assert ok is False and "backend on fire" in reason


def test_matches_url_only_for_this_market(env, backend):
    prov = provider(backend)
    assert prov.matches_url("http://market.test/skills/demo/1.0.0")
    assert not prov.matches_url("https://clawhub.ai/skills/demo")
    assert not prov.matches_url("")


def test_fetch_bundle_parses_url_and_returns_host_shape(env, backend):
    backend.on(
        "GET",
        "/skills/demo/1.0.0",
        200,
        {
            "name": "Demo Skill",
            "files": {"SKILL.md": "---\nname: demo\ndescription: x\n---\n"},
            "source_url": "http://market.test/skills/demo/1.0.0",
        },
    )
    prov = provider(backend)
    bundle, resolved = asyncio.run(prov.fetch_bundle("http://market.test/skills/demo/1.0.0", "1.0.0"))
    assert bundle["name"] == "Demo Skill"
    assert "SKILL.md" in bundle["files"]
    assert resolved.endswith("/skills/demo/1.0.0")


def test_fetch_bundle_requested_version_overrides_url(env, backend):
    backend.on("GET", "/skills/demo/2.0.0", 200, {"name": "Demo", "files": {"SKILL.md": "#"}})
    prov = provider(backend)
    asyncio.run(prov.fetch_bundle("http://market.test/skills/demo/1.0.0", "2.0.0"))
    assert backend.requests[-1]["path"] == "/skills/demo/2.0.0"


def test_fetch_bundle_falls_back_to_latest(env, backend):
    backend.on("GET", "/skills/demo/latest", 200, {"name": "Demo", "files": {"SKILL.md": "#"}})
    prov = provider(backend)
    asyncio.run(prov.fetch_bundle("http://market.test/skills/demo", ""))
    assert backend.requests[-1]["path"] == "/skills/demo/latest"


def test_fetch_bundle_rejects_malformed_payload(env, backend):
    backend.on("GET", "/skills/demo/1.0.0", 200, {"name": "Demo"})
    prov = provider(backend)
    with pytest.raises(BackendError):
        asyncio.run(prov.fetch_bundle("http://market.test/skills/demo/1.0.0", "1.0.0"))


def test_fetch_bundle_refuses_binary_attachments(env, backend):
    """Hub install writes text; base64 must not silently become a corrupt file."""
    backend.on(
        "GET",
        "/skills/demo/1.0.0",
        200,
        {
            "name": "Demo",
            "files": {
                "SKILL.md": "---\nname: demo\ndescription: x\n---\n",
                "docs/report.docx": {"base64": "UEsDBP/+AA=="},
            },
        },
    )
    prov = provider(backend)
    with pytest.raises(BackendError) as excinfo:
        asyncio.run(prov.fetch_bundle("http://market.test/skills/demo/1.0.0", "1.0.0"))
    assert excinfo.value.code == "binary_bundle"
    assert "report.docx" in str(excinfo.value)


def test_fetch_bundle_propagates_takedown(env, backend):
    backend.on("GET", "/skills/demo/1.0.0", 410, {"error": {"code": "taken_down", "message": "gone"}})
    prov = provider(backend)
    with pytest.raises(BackendError) as excinfo:
        asyncio.run(prov.fetch_bundle("http://market.test/skills/demo/1.0.0", "1.0.0"))
    assert excinfo.value.status == 410


def test_business_errors_do_not_mark_the_market_unavailable(env, backend):
    """403/410 are normal answers; only outages may blank the market."""
    backend.on("GET", "/skills/demo/1.0.0", 410, {"error": {"code": "taken_down", "message": "gone"}})
    prov = provider(backend)
    with pytest.raises(BackendError):
        asyncio.run(prov.fetch_bundle("http://market.test/skills/demo/1.0.0", "1.0.0"))
    assert prov.available() == (True, None)

    backend.on("GET", "/api/v1/skills", 503, {"error": {"code": "down", "message": "backend down"}})
    with pytest.raises(BackendError):
        asyncio.run(prov.search("", 10, 1))
    ok, reason = prov.available()
    assert ok is False and "backend down" in reason
