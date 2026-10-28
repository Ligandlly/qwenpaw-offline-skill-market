# -*- coding: utf-8 -*-
"""Agent-market routes: thin proxies, snapshot publish, install delegation."""

from __future__ import annotations

import pytest

fastapi = pytest.importorskip("fastapi")
from fastapi import FastAPI  # noqa: E402
from fastapi.testclient import TestClient  # noqa: E402

from offline_market.agents import AgentError, InstalledLedger  # noqa: E402
from offline_market.client import BackendClient  # noqa: E402
from offline_market.host import HostError  # noqa: E402
from offline_market.routes import build_router  # noqa: E402
from offline_market.runtime import PluginSettings  # noqa: E402


class RouteHost:
    def __init__(self, agents=None, error: Exception | None = None) -> None:
        self.agents = agents or []
        self.error = error
        self.deleted: list[str] = []

    def list_agents(self):
        if self.error:
            raise self.error
        return self.agents

    def delete_agent(self, agent_id):
        self.deleted.append(agent_id)
        return {"success": True}


@pytest.fixture
def api(backend, monkeypatch):
    settings = PluginSettings()
    app = FastAPI()
    app.include_router(build_router(settings), prefix="/offline-market")
    monkeypatch.setattr(
        "offline_market.routes.BackendClient",
        lambda url, tenant, timeout=10.0: BackendClient(url, tenant, transport=backend),
    )
    return TestClient(app)


@pytest.fixture
def host(monkeypatch):
    fake = RouteHost()
    monkeypatch.setattr("offline_market.routes.HostClient", lambda *a, **k: fake)
    return fake


def test_agent_listing_and_mine_proxy(api, env, backend, host):
    backend.on("GET", "/api/v1/agents", 200, {"items": [{"slug": "qa"}]})
    backend.on("GET", "/api/v1/agents/mine", 200, {"items": []})

    response = api.get("/offline-market/agents?q=qa&page=2&limit=5")
    assert response.status_code == 200
    assert backend.requests[-1]["path"] == "/api/v1/agents?q=qa&page=2&limit=5&sort=created"
    assert backend.requests[-1]["headers"]["x-tenant-id"] == "tenant-a"

    assert api.get("/offline-market/agents/mine").json() == {"items": []}


def test_host_agents_only_lists_qwenpaw_backends(api, env, monkeypatch):
    fake = RouteHost(
        agents=[
            {"id": "qa", "name": "QA", "backend": "qwenpaw", "enabled": True},
            {"id": "codex", "name": "Codex", "backend": "codex"},
        ]
    )
    monkeypatch.setattr("offline_market.routes.HostClient", lambda *a, **k: fake)
    response = api.get("/offline-market/host-agents")
    assert response.status_code == 200, response.text
    items = response.json()["items"]
    assert [item["id"] for item in items] == ["qa"]


def test_host_health_reports_the_self_call(api, env, monkeypatch):
    class HealthHost:
        base_url = "http://127.0.0.1:8088"
        runtime_token = "token"

        def list_agents(self):
            return [{"id": "qa"}, {"id": "other"}]

    monkeypatch.setattr("offline_market.routes.HostClient", lambda *a, **k: HealthHost())
    body = api.get("/offline-market/host-health").json()
    assert body["ok"] is True
    assert body["agent_count"] == 2
    assert body["host_api_url"] == "http://127.0.0.1:8088"
    assert body["runtime_token_present"] is True


def test_host_health_returns_200_with_failure_detail(api, env, monkeypatch):
    fake = RouteHost(error=HostError(401, "invalid_runtime_token", "bad"))
    monkeypatch.setattr("offline_market.routes.HostClient", lambda *a, **k: fake)
    response = api.get("/offline-market/host-health")
    assert response.status_code == 200
    assert response.json()["ok"] is False
    assert response.json()["code"] == "invalid_runtime_token"


def test_publish_snapshots_then_forwards_the_bundle(
    api, env, backend, monkeypatch
):
    seen = {}

    def fake_snapshot(host, agent_id):
        seen["agent_id"] = agent_id
        return {
            "manifest": {"schema": 1, "name": "QA"},
            "files": {"manifest.json": "{}", "SOUL.md": "# soul"},
        }

    monkeypatch.setattr("offline_market.routes.snapshot_agent", fake_snapshot)
    backend.on(
        "POST", "/api/v1/agents", 201, {"slug": "qa", "version": "1.0.0"}
    )

    response = api.post(
        "/offline-market/agents/publish",
        json={
            "source_agent_id": "qa",
            "slug": "qa",
            "version": "1.0.0",
            "visibility": "tenant",
            "publisher_name": "  admin\n",
        },
    )
    assert response.status_code == 200, response.text
    assert seen["agent_id"] == "qa"
    sent = backend.requests[-1]["body"]
    assert sent["slug"] == "qa"
    assert sent["visibility"] == "tenant"
    assert sent["publisher_name"] == "admin"
    assert sent["files"] == {"manifest.json": "{}", "SOUL.md": "# soul"}


def test_publish_requires_source_and_version(api, env, backend):
    response = api.post(
        "/offline-market/agents/publish", json={"version": "1.0.0"}
    )
    assert response.status_code == 400
    assert response.json()["detail"]["code"] == "invalid_publish"
    assert not backend.requests


def test_install_fetches_the_bundle_and_delegates(
    api, env, backend, monkeypatch
):
    bundle = {"version": "1.0.0", "files": {"manifest.json": "{}"}}
    backend.on(
        "GET", "/api/v1/agents/qa/latest/bundle", 200, bundle
    )
    captured = {}

    class FakeInstaller:
        def install(self, *, slug, bundle, target_name):
            captured.update(slug=slug, bundle=bundle, target_name=target_name)
            return {"agent_id": "qa", "name": target_name}

    monkeypatch.setattr(
        "offline_market.routes.build_installer",
        lambda settings, host: FakeInstaller(),
    )

    response = api.post(
        "/offline-market/agents/qa/install",
        json={"version": "latest", "target_name": "  内网 QA  "},
    )
    assert response.status_code == 200, response.text
    assert captured == {
        "slug": "qa",
        "bundle": bundle,
        "target_name": "内网 QA",
    }


def test_install_failure_maps_status_and_step(api, env, backend, monkeypatch):
    backend.on(
        "GET",
        "/api/v1/agents/qa/latest/bundle",
        200,
        {"version": "1.0.0", "files": {"manifest.json": "{}"}},
    )

    class FailingInstaller:
        def install(self, **_kwargs):
            error = AgentError(
                "already_installed", "已经装过", status=409, step="create_agent"
            )
            error.workspace_kept = "/tmp/ws"
            raise error

    monkeypatch.setattr(
        "offline_market.routes.build_installer",
        lambda settings, host: FailingInstaller(),
    )
    response = api.post("/offline-market/agents/qa/install", json={})
    assert response.status_code == 409
    detail = response.json()["detail"]
    assert detail["code"] == "already_installed"
    assert detail["step"] == "create_agent"
    assert detail["workspace_kept"] == "/tmp/ws"


def test_uninstall_requires_a_ledger_entry(api, env, host):
    store = InstalledLedger(PluginSettings().state_dir / "installed-agents.json")
    store.add(
        {
            "agent_id": "qa",
            "slug": "qa",
            "version": "1.0.0",
            "name": "QA",
            "workspace_dir": "/tmp/ws-qa",
            "installed_at": 1,
        }
    )
    response = api.post("/offline-market/agents/qa/uninstall")
    assert response.status_code == 200, response.text
    assert response.json()["workspace_kept"] == "/tmp/ws-qa"
    assert host.deleted == ["qa"]
    assert store.get("qa") is None

    missing = api.post("/offline-market/agents/unknown/uninstall")
    assert missing.status_code == 404
    assert missing.json()["detail"]["code"] == "not_owned"


def test_installed_reconciles_with_the_host_and_reports_state(
    api, env, backend, host
):
    store = InstalledLedger(PluginSettings().state_dir / "installed-agents.json")
    store.add(
        {
            "agent_id": "qa",
            "slug": "qa",
            "version": "1.0.0",
            "name": "QA",
            "workspace_dir": "/tmp/ws-qa",
            "installed_at": 1,
        }
    )
    host.agents = [{"id": "qa"}]
    backend.on(
        "GET", "/api/v1/agents/qa/status", 200, {"state": "withdrawn"}
    )
    items = api.get("/offline-market/agents/installed").json()["items"]
    assert items[0]["agent_id"] == "qa"
    assert items[0]["state"] == "withdrawn"


def test_preview_returns_counts_without_publishing(api, env, backend, monkeypatch):
    monkeypatch.setattr(
        "offline_market.routes.snapshot_agent",
        lambda host, agent_id: {
            "manifest": {
                "schema": 1,
                "name": "QA",
                "md_files": ["SOUL.md"],
                "skills": ["demo"],
            },
            "files": {
                "manifest.json": "{}",
                "SOUL.md": "abc",
                "skills/demo/SKILL.md": "x",
            },
        },
    )
    response = api.post(
        "/offline-market/agents/preview", json={"source_agent_id": "qa"}
    )
    assert response.status_code == 200, response.text
    body = response.json()
    assert body["md_files"] == ["SOUL.md"]
    assert body["skills"] == ["demo"]
    assert body["file_count"] == 3
    assert body["size_bytes"] == 6
    assert not backend.requests  # preview never reaches the market backend


def test_host_errors_map_to_http_status(api, env, monkeypatch):
    fake = RouteHost(
        error=HostError(401, "invalid_runtime_token", "bad runtime token")
    )
    monkeypatch.setattr("offline_market.routes.HostClient", lambda *a, **k: fake)
    response = api.get("/offline-market/host-agents")
    assert response.status_code == 401
    assert response.json()["detail"]["code"] == "invalid_runtime_token"
