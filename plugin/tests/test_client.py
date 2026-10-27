# -*- coding: utf-8 -*-
"""Transport error mapping: every network failure must become a BackendError.

`urllib` 把连接失败包成 URLError，但**读**超时是裸的 `TimeoutError`（3.10 起
`socket.timeout` 就是 `TimeoutError`），不接住就会变成宿主侧的 500 而不是 502。
"""

from __future__ import annotations

import urllib.error
import urllib.request

import pytest

from offline_market.client import BackendError, urllib_transport

URL = "http://market.test:8080/api/v1/skills"


def _raise(monkeypatch, exc: BaseException):
    def boom(*_args, **_kwargs):
        raise exc

    monkeypatch.setattr(urllib.request, "urlopen", boom)


def test_connect_failure_names_the_url(monkeypatch):
    _raise(monkeypatch, urllib.error.URLError("connection refused"))
    with pytest.raises(BackendError) as excinfo:
        urllib_transport("GET", URL, {}, None, 1.0)
    assert excinfo.value.status == 0
    assert excinfo.value.code == "unreachable"
    assert URL in str(excinfo.value)


def test_read_timeout_is_not_a_crash(monkeypatch):
    _raise(monkeypatch, TimeoutError("timed out"))
    with pytest.raises(BackendError) as excinfo:
        urllib_transport("GET", URL, {}, None, 1.0)
    assert excinfo.value.status == 0
    assert excinfo.value.code == "timeout"
    assert URL in str(excinfo.value)


def test_http_error_keeps_its_body(monkeypatch):
    _raise(monkeypatch, urllib.error.HTTPError(URL, 503, "boom", {}, None))
    status, raw = urllib_transport("GET", URL, {}, None, 1.0)
    assert status == 503
    assert raw == b""


def test_invalid_url_is_a_backend_error_not_a_crash():
    with pytest.raises(BackendError) as excinfo:
        urllib_transport("GET", "http://10 .10.145.48:8080/health", {}, None, 1.0)
    assert excinfo.value.status == 0
    assert excinfo.value.code == "invalid_market_url"
    assert "10 .10.145.48" in str(excinfo.value)
