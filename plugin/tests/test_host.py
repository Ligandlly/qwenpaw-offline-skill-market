# -*- coding: utf-8 -*-
"""Host API client: base-URL policy, auth forwarding, multipart, errors."""

from __future__ import annotations

import json

import pytest

from offline_market.host import (
    HostClient,
    HostError,
    resolve_base_url,
    validate_base_url,
)
from offline_market.runtime import PluginSettings


class FakeTransport:
    def __init__(self, status: int = 200, payload: object | None = None) -> None:
        self.status = status
        self.payload = payload if payload is not None else {}
        self.calls: list[dict] = []

    def __call__(self, method, url, headers, body, timeout):
        self.calls.append(
            {
                "method": method,
                "url": url,
                "headers": dict(headers),
                "body": body,
                "timeout": timeout,
            }
        )
        return self.status, json.dumps(self.payload).encode("utf-8")


def test_base_url_precedence(env):
    env.setenv("QWENPAW_PORT", "9999")
    assert resolve_base_url(PluginSettings()) == "http://127.0.0.1:9999"

    env.setenv("QWENPAW_RUNTIME_API_URL", "http://127.0.0.1:9001")
    assert resolve_base_url(PluginSettings()) == "http://127.0.0.1:9001"

    env.setenv("OFFLINE_MARKET_HOST_API_URL", "http://127.0.0.1:9002")
    assert resolve_base_url(PluginSettings()) == "http://127.0.0.1:9002"

    settings = PluginSettings({"host_api_url": "http://127.0.0.1:9003"})
    assert resolve_base_url(settings) == "http://127.0.0.1:9003"


def test_runtime_url_must_be_loopback_or_it_is_skipped(env):
    env.setenv("QWENPAW_PORT", "9999")
    env.setenv("QWENPAW_RUNTIME_API_URL", "http://hub.example:9001")
    assert resolve_base_url(PluginSettings()) == "http://127.0.0.1:9999"


def test_request_base_url_is_the_last_resort(env):
    env.delenv("QWENPAW_RUNTIME_API_URL", raising=False)
    env.delenv("OFFLINE_MARKET_HOST_API_URL", raising=False)
    env.setenv("QWENPAW_PORT", "not-a-port")
    assert (
        resolve_base_url(PluginSettings(), request_base_url="http://127.0.0.1:8123/")
        == "http://127.0.0.1:8123"
    )


def test_explicit_port_wins_over_a_browser_mapped_port(env):
    env.delenv("QWENPAW_RUNTIME_API_URL", raising=False)
    env.delenv("OFFLINE_MARKET_HOST_API_URL", raising=False)
    env.setenv("QWENPAW_PORT", "9999")
    assert (
        resolve_base_url(PluginSettings(), request_base_url="http://127.0.0.1:49154/")
        == "http://127.0.0.1:9999"
    )


def test_loopback_request_base_wins_when_port_is_unset(env):
    env.delenv("QWENPAW_RUNTIME_API_URL", raising=False)
    env.delenv("OFFLINE_MARKET_HOST_API_URL", raising=False)
    env.delenv("QWENPAW_PORT", raising=False)
    assert (
        resolve_base_url(PluginSettings(), request_base_url="http://127.0.0.1:8123/")
        == "http://127.0.0.1:8123"
    )


def test_refused_published_port_falls_back_to_the_local_listener():
    calls: list[str] = []

    def transport(method, url, headers, body, timeout):
        calls.append(url)
        if len(calls) == 1:
            raise HostError(
                0,
                "unreachable",
                "宿主 API 不可达（http://127.0.0.1:49154/api/agents）"
                "：[Errno 111] Connection refused",
            )
        return 200, json.dumps({"agents": [{"id": "qa"}]}).encode("utf-8")

    client = HostClient(
        "http://127.0.0.1:49154",
        runtime_token="runtime-token",
        transport=transport,
        fallback_base_url="http://127.0.0.1:8088",
    )
    assert client.list_agents() == [{"id": "qa"}]
    assert calls == [
        "http://127.0.0.1:49154/api/agents",
        "http://127.0.0.1:8088/api/agents",
    ]
    assert client.base_url == "http://127.0.0.1:8088"
    # The successful fallback is remembered; no more refused attempts.
    assert client.list_agents() == [{"id": "qa"}]
    assert calls[-1] == "http://127.0.0.1:8088/api/agents"


def test_plain_http_is_loopback_only():
    with pytest.raises(HostError) as excinfo:
        validate_base_url("http://hub.example:8080")
    assert excinfo.value.code == "insecure_host_api"

    assert validate_base_url("http://host.docker.internal:8088") == (
        "http://host.docker.internal:8088"
    )
    assert validate_base_url("https://hub.example:8080") == "https://hub.example:8080"
    assert validate_base_url("http://[::1]:8088") == "http://[::1]:8088"


def test_credentials_and_paths_are_rejected():
    for url in (
        "http://user:pass@127.0.0.1:8088",
        "http://127.0.0.1:8088/api",
        "http://127.0.0.1:8088?x=1",
    ):
        with pytest.raises(HostError):
            validate_base_url(url)


def test_requests_forward_both_tokens_and_json():
    transport = FakeTransport(payload={"agents": [{"id": "qa"}]})
    client = HostClient(
        "http://127.0.0.1:8088",
        authorization="Bearer user-token",
        runtime_token="runtime-token",
        transport=transport,
    )
    assert client.list_agents() == [{"id": "qa"}]
    call = transport.calls[-1]
    assert call["url"] == "http://127.0.0.1:8088/api/agents"
    assert call["headers"]["authorization"] == "Bearer user-token"
    assert call["headers"]["x-qwenpaw-runtime-token"] == "runtime-token"


def test_host_error_detail_shapes_are_preserved():
    error = HostClient(
        "http://127.0.0.1:8088",
        transport=FakeTransport(422, {"detail": "SKILL.md must include name"}),
    )
    with pytest.raises(HostError) as excinfo:
        error.list_agents()
    assert excinfo.value.status == 422
    assert "SKILL.md" in str(excinfo.value)

    structured = HostClient(
        "http://127.0.0.1:8088",
        transport=FakeTransport(403, {"detail": {"code": "forbidden", "message": "no"}}),
    )
    with pytest.raises(HostError) as excinfo:
        structured.list_agents()
    assert excinfo.value.code == "forbidden"


def test_skill_upload_is_multipart_and_asks_for_enable():
    transport = FakeTransport(payload={"imported": ["demo"], "count": 1})
    client = HostClient("http://127.0.0.1:8088", transport=transport)
    payload = b"PK\x03\x04fake-zip"
    assert client.upload_skills_zip("qa", payload)["count"] == 1

    call = transport.calls[-1]
    assert call["method"] == "POST"
    assert call["url"].endswith(
        "/api/agents/qa/skills/upload?enable=true"
    )
    assert call["headers"]["content-type"].startswith("multipart/form-data; boundary=")
    assert b'name="file"' in call["body"]
    assert b'filename="skills.zip"' in call["body"]
    assert b"PK\x03\x04fake-zip" in call["body"]
    assert call["timeout"] >= 120


def test_runtime_token_is_not_sent_to_an_https_origin():
    transport = FakeTransport(payload={"agents": []})
    client = HostClient(
        "https://hub.example",
        authorization="Bearer user-token",
        runtime_token="runtime-token",
        transport=transport,
    )
    client.list_agents()
    headers = transport.calls[-1]["headers"]
    assert headers["authorization"] == "Bearer user-token"
    assert "x-qwenpaw-runtime-token" not in headers


def test_toggle_has_no_body():
    transport = FakeTransport(payload={"name": "browser", "enabled": False})
    client = HostClient("http://127.0.0.1:8088", transport=transport)
    client.toggle_tool("qa", "browser")
    call = transport.calls[-1]
    assert call["method"] == "PATCH"
    assert call["body"] is None
