# -*- coding: utf-8 -*-
"""Our own routes: what the sidebar page (M3) will call."""

from __future__ import annotations

import io
import zipfile

import pytest
from urllib.parse import quote

fastapi = pytest.importorskip("fastapi")
from fastapi import FastAPI  # noqa: E402
from fastapi.testclient import TestClient  # noqa: E402

from offline_market.client import BackendClient  # noqa: E402
from offline_market.routes import build_router  # noqa: E402
from offline_market.runtime import PluginSettings  # noqa: E402


def make_zip(entries: dict[str, str | bytes]) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in entries.items():
            archive.writestr(name, data)
    return buffer.getvalue()


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


def test_config_endpoint_reports_gate_and_problems(api, env):
    body = api.get("/offline-market/config").json()
    assert body["market_url"] == "http://market.test"
    assert body["tenant"] == "tenant-a"
    assert body["scan_mode"] == "block" and body["scan_mode_ok"] is True
    assert body["problems"] == []


def test_routes_are_unavailable_when_not_configured(api, env):
    env.delenv("QWENPAW_TENANT_ID")
    response = api.get("/offline-market/mine")
    assert response.status_code == 503
    assert response.json()["detail"]["code"] == "not_configured"


def test_list_and_mine_pass_the_tenant_header(api, env, backend):
    backend.on("GET", "/api/v1/skills", 200, {"items": []})
    backend.on("GET", "/api/v1/skills/mine", 200, {"items": [{"slug": "x"}]})
    assert api.get("/offline-market/skills?q=demo&page=2&limit=5").status_code == 200
    assert backend.requests[-1]["path"] == "/api/v1/skills?q=demo&page=2&limit=5"
    assert backend.requests[-1]["headers"]["x-tenant-id"] == "tenant-a"

    assert api.get("/offline-market/mine").json()["items"][0]["slug"] == "x"


def test_publish_forwards_visibility_and_rejects_bad_files(api, env, backend):
    backend.on("POST", "/api/v1/skills", 201, {"slug": "demo", "version": "1.0.0"})
    ok = api.post(
        "/offline-market/skills",
        json={
            "name": "Demo",
            "version": "1.0.0",
            "visibility": "tenant",
            "files": {"SKILL.md": "---\nname: demo\ndescription: x\n---\n"},
        },
    )
    assert ok.status_code == 200
    sent = backend.requests[-1]["body"]
    assert sent["visibility"] == "tenant"
    assert sent["slug"] is None
    assert backend.requests[-1]["headers"]["x-tenant-id"] == "tenant-a"

    bad = api.post("/offline-market/skills", json={"name": "Demo", "version": "1.0.0", "files": "nope"})
    assert bad.status_code == 400
    assert bad.json()["detail"]["code"] == "invalid_files"

    # Binary is `{"base64": ...}`; a text-looking object with other keys is not.
    binary = api.post(
        "/offline-market/skills",
        json={
            "name": "Demo",
            "version": "1.0.0",
            "files": {
                "SKILL.md": "---\nname: demo\ndescription: x\n---\n",
                "assets/doc.pdf": {"data": "nope"},
            },
        },
    )
    assert binary.status_code == 400
    assert binary.json()["detail"]["code"] == "invalid_files"

    missing = api.post(
        "/offline-market/skills", json={"name": "Demo", "files": {"SKILL.md": "#"}}
    )
    assert missing.status_code == 400
    assert "version" in missing.json()["detail"]["message"]


def test_publish_zip_extracts_the_archive_and_forwards_the_files(api, env, backend):
    backend.on(
        "POST",
        "/api/v1/skills",
        201,
        {"slug": "demo", "version": "1.0.0", "file_count": 2},
    )
    response = api.post(
        "/offline-market/skills/zip?name=Demo&version=1.0.0&visibility=tenant",
        content=make_zip(
            {
                "my-skill/SKILL.md": "---\nname: demo\ndescription: x\n---\n",
                "my-skill/scripts/run.py": "print(1)\n",
            }
        ),
        headers={"content-type": "application/zip"},
    )
    assert response.status_code == 200
    assert response.json()["file_count"] == 2
    sent = backend.requests[-1]["body"]
    assert sent["name"] == "Demo"
    assert sent["visibility"] == "tenant"
    assert sent["files"] == {
        "SKILL.md": "---\nname: demo\ndescription: x\n---\n",
        "scripts/run.py": "print(1)\n",
    }


def test_publish_zip_forwards_binary_files_as_base64(api, env, backend):
    backend.on("POST", "/api/v1/skills", 201, {"slug": "demo", "version": "1.0.0"})
    pdf = b"%PDF-1.7\x00\xff\xfe"
    response = api.post(
        "/offline-market/skills/zip?name=Demo&version=1.0.0",
        content=make_zip(
            {
                "SKILL.md": "---\nname: demo\ndescription: x\n---\n",
                "references/manual.pdf": pdf,
            }
        ),
        headers={"content-type": "application/zip"},
    )
    assert response.status_code == 200
    sent = backend.requests[-1]["body"]["files"]
    assert sent["references/manual.pdf"] == {"base64": "JVBERi0xLjcA//4="}


def test_publish_zip_rejects_broken_archives_and_missing_fields(api, env, backend):
    broken = api.post(
        "/offline-market/skills/zip?name=Demo&version=1.0.0",
        content=b"not a zip",
        headers={"content-type": "application/zip"},
    )
    assert broken.status_code == 400
    assert broken.json()["detail"]["code"] == "invalid_zip"

    no_manifest = api.post(
        "/offline-market/skills/zip?name=Demo&version=1.0.0",
        content=make_zip({"README.md": "hi"}),
        headers={"content-type": "application/zip"},
    )
    assert no_manifest.status_code == 400
    assert no_manifest.json()["detail"]["code"] == "missing_manifest"

    no_version = api.post(
        "/offline-market/skills/zip?name=Demo",
        content=make_zip({"SKILL.md": "#"}),
        headers={"content-type": "application/zip"},
    )
    assert no_version.status_code == 400
    assert "version" in no_version.json()["detail"]["message"]
    assert not backend.requests  # nothing ever reached the backend


def test_publish_forwards_the_publisher_name_and_clamps_it(api, env, backend):
    backend.on("POST", "/api/v1/skills", 201, {"slug": "demo", "version": "1.0.0"})
    manifest = "---\nname: demo\ndescription: x\n---\n"

    api.post(
        "/offline-market/skills",
        json={
            "name": "Demo",
            "version": "1.0.0",
            "files": {"SKILL.md": manifest},
            "publisher_name": "  admin\n",
        },
    )
    assert backend.requests[-1]["body"]["publisher_name"] == "admin"

    # the zip route takes it as a query parameter, clamped to 64 characters
    api.post(
        f"/offline-market/skills/zip?name=Demo&version=1.0.0&publisher_name={quote('张' * 100)}",
        content=make_zip({"SKILL.md": manifest}),
        headers={"content-type": "application/zip"},
    )
    assert backend.requests[-1]["body"]["publisher_name"] == "张" * 64

    # an empty name is not sent at all: the market keeps what it already has
    api.post(
        "/offline-market/skills",
        json={
            "name": "Demo",
            "version": "1.0.0",
            "files": {"SKILL.md": manifest},
            "publisher_name": "   ",
        },
    )
    assert "publisher_name" not in backend.requests[-1]["body"]

    # and neither is a non-string one
    api.post(
        "/offline-market/skills/zip?name=Demo&version=1.0.0",
        content=make_zip({"SKILL.md": manifest}),
        headers={"content-type": "application/zip"},
    )
    assert "publisher_name" not in backend.requests[-1]["body"]


def test_backend_errors_pass_through_with_their_status(api, env, backend):
    backend.on(
        "GET",
        "/api/v1/skills/secret/status",
        403,
        {"error": {"code": "forbidden", "message": "private"}},
    )
    response = api.get("/offline-market/skills/secret/status")
    assert response.status_code == 403
    assert response.json()["detail"] == {
        "code": "forbidden",
        "message": "private",
        "backend_status": 403,
    }


def test_unreachable_backend_becomes_502(api, env, monkeypatch):
    def boom(method, url, headers, body, timeout):
        from offline_market.client import BackendError

        raise BackendError(0, "unreachable", "市场后端不可达：连接被拒绝")

    monkeypatch.setattr(
        "offline_market.routes.BackendClient",
        lambda url, tenant, timeout=10.0: BackendClient(url, tenant, transport=boom),
    )
    response = api.get("/offline-market/mine")
    assert response.status_code == 502
    assert response.json()["detail"]["code"] == "unreachable"


def test_withdraw_and_restore(api, env, backend):
    backend.on("POST", "/api/v1/skills/demo/withdraw", 200, {"state": "withdrawn"})
    backend.on("POST", "/api/v1/skills/demo/restore", 200, {"state": "active"})
    assert api.post("/offline-market/skills/demo/withdraw", json={"reason": "oops"}).json()["state"] == "withdrawn"
    assert backend.requests[-1]["body"] == {"reason": "oops"}
    assert api.post("/offline-market/skills/demo/restore").json()["state"] == "active"


def test_lookup_forwards_display_names(api, env, backend):
    backend.on(
        "GET",
        "/api/v1/skills/lookup",
        200,
        {"items": [{"name": "Demo Skill", "slug": "demo", "state": "taken_down"}]},
    )
    response = api.get("/offline-market/lookup?names=Demo%20Skill,Other")
    assert response.status_code == 200
    assert response.json()["items"][0]["state"] == "taken_down"
    assert backend.requests[-1]["path"].startswith("/api/v1/skills/lookup?names=")

    # nothing to look up -> no backend round trip
    before = len(backend.requests)
    assert api.get("/offline-market/lookup?names=").json() == {"items": []}
    assert len(backend.requests) == before
