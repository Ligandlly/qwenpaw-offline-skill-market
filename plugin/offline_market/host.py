# -*- coding: utf-8 -*-
"""Client for *this runtime's own* QwenPaw HTTP API.

The plugin runs inside the host process, but agent creation/installation is
deliberately done over HTTP rather than by importing host internals:

* the host keeps its config transaction, agent startup, skill scanning and
  workspace initialisation in one place;
* M2's install sequence is observable/testable with an injected transport;
* the same code works for Hub-local, Docker and standalone runtimes.

Identity: the browser's ``Authorization`` header is forwarded unchanged, and
``QWENPAW_RUNTIME_INTERNAL_TOKEN`` is added when the runtime has one (Hub
injects it and ``RuntimeBoundaryMiddleware`` requires it).

stdlib only, loopback (or ``host.docker.internal``) only for plain HTTP: the
internal token must never be sent to an arbitrary host.
"""

from __future__ import annotations

import ipaddress
import json
import os
import secrets
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Callable

# (method, url, headers, body-or-None, timeout) -> (status, raw body)
Transport = Callable[[str, str, dict[str, str], bytes | None, float], tuple[int, bytes]]

_RUNTIME_TOKEN_ENV = "QWENPAW_RUNTIME_INTERNAL_TOKEN"
_ALLOWED_PLAIN_HOSTS = frozenset({"localhost", "host.docker.internal"})


class HostError(RuntimeError):
    """The host answered with something we cannot use."""

    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.code = code

    def __str__(self) -> str:  # host displays this verbatim in some paths
        return str(self.args[0]) if self.args else self.code


def urllib_transport(
    method: str,
    url: str,
    headers: dict[str, str],
    body: bytes | None,
    timeout: float,
) -> tuple[int, bytes]:
    request = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:  # noqa: S310
            return response.status, response.read()
    except urllib.error.HTTPError as exc:  # 4xx/5xx still carry a JSON body
        return exc.code, exc.read()
    except urllib.error.URLError as exc:
        raise HostError(0, "unreachable", f"宿主 API 不可达（{url}）：{exc.reason}") from exc
    except TimeoutError as exc:
        raise HostError(0, "timeout", f"宿主 API 超时（{url}）：{exc}") from exc
    except OSError as exc:
        raise HostError(0, "unreachable", f"宿主 API 不可达（{url}）：{exc}") from exc


def _is_connection_refused(exc: HostError) -> bool:
    """True only when nothing reached the server (so a retry is safe)."""
    if exc.code != "unreachable":
        return False
    text = str(exc).lower()
    return (
        "refused" in text
        or "errno 111" in text
        or "错误号 111" in text
        or "拒绝" in text
    )


def _is_loopback(host: str) -> bool:
    if host.lower() in {"localhost", "::1"}:
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def validate_base_url(raw: str) -> str:
    """Accept an origin, never a URL with credentials/path/query."""
    text = (raw or "").strip()
    if not text:
        raise HostError(0, "no_host_api", "宿主 API 地址为空")
    parts = urllib.parse.urlsplit(text)
    if parts.scheme not in {"http", "https"} or not parts.hostname:
        raise HostError(0, "invalid_host_api", f"宿主 API 地址不合法：{raw}")
    if parts.username or parts.password:
        raise HostError(0, "invalid_host_api", "宿主 API 地址不能包含凭据")
    if parts.path not in {"", "/"} or parts.query or parts.fragment:
        raise HostError(0, "invalid_host_api", "宿主 API 地址只能是 origin（不能带路径/查询）")
    host = parts.hostname
    if parts.scheme == "http" and not (
        _is_loopback(host) or host.lower() in _ALLOWED_PLAIN_HOSTS
    ):
        raise HostError(
            0,
            "insecure_host_api",
            f"拒绝向非 loopback 地址发送内部令牌：{host}",
        )
    return f"{parts.scheme}://{parts.netloc}".rstrip("/")


def default_local_base_url() -> str:
    """The listener this process actually owns (container port, not published)."""
    port = os.environ.get("QWENPAW_PORT", "").strip()
    if not port.isdigit():
        port = "8088"
    return validate_base_url(f"http://127.0.0.1:{port}")


def resolve_base_url(settings, request_base_url: str | None = None) -> str:
    """Where this runtime's own API lives.

    Order: explicit config/env > Hub-injected runtime URL > an explicit
    ``QWENPAW_PORT`` (the real listener; in Docker the browser-facing port is
    the host's published mapping) > a loopback browser-facing base URL (custom
    standalone port) > the CLI/container default.
    """
    explicit = settings.host_api_url
    if explicit:
        return validate_base_url(explicit)

    runtime_url = os.environ.get("QWENPAW_RUNTIME_API_URL", "").strip()
    if runtime_url:
        try:
            return validate_base_url(runtime_url)
        except HostError:
            # A Hub-supplied URL that is not loopback is not ours to trust;
            # fall through to the local listener instead of failing the page.
            pass

    port = os.environ.get("QWENPAW_PORT", "").strip()
    if port.isdigit():
        return validate_base_url(f"http://127.0.0.1:{port}")

    if request_base_url:
        try:
            candidate = validate_base_url(request_base_url)
        except HostError:
            candidate = ""
        if candidate and _is_loopback(urllib.parse.urlsplit(candidate).hostname or ""):
            return candidate

    return default_local_base_url()


def _multipart(field: str, filename: str, content_type: str, data: bytes) -> tuple[bytes, str]:
    boundary = f"----offline-market-{secrets.token_hex(12)}"
    head = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="{field}"; filename="{filename}"\r\n'
        f"Content-Type: {content_type}\r\n\r\n"
    ).encode("utf-8")
    tail = f"\r\n--{boundary}--\r\n".encode("utf-8")
    return head + data + tail, f"multipart/form-data; boundary={boundary}"


class HostClient:
    def __init__(
        self,
        base_url: str,
        *,
        authorization: str | None = None,
        runtime_token: str | None = None,
        timeout: float = 30.0,
        transport: Transport | None = None,
        fallback_base_url: str | None = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self._fallback_base_url = (
            fallback_base_url.rstrip("/") if fallback_base_url else None
        )
        self.authorization = authorization or None
        # The runtime boundary token is for our own listener only. An explicit
        # HTTPS hub origin may legitimately proxy us, but it must never receive
        # this token.
        host = urllib.parse.urlsplit(self.base_url).hostname or ""
        self.runtime_token = (
            runtime_token
            if runtime_token and (_is_loopback(host) or host.lower() in _ALLOWED_PLAIN_HOSTS)
            else None
        )
        self.timeout = timeout
        self._transport = transport or urllib_transport

    # ---------------------------------------------------------- plumbing ---

    def _url(
        self, path: str, query: dict[str, Any] | None = None, base: str | None = None
    ) -> str:
        url = f"{(base or self.base_url).rstrip('/')}{path}"
        if query:
            clean = {k: str(v) for k, v in query.items() if v is not None and v != ""}
            if clean:
                url = f"{url}?{urllib.parse.urlencode(clean)}"
        return url

    def _request(
        self,
        method: str,
        path: str,
        *,
        query: dict[str, Any] | None = None,
        body: Any = None,
        data: bytes | None = None,
        content_type: str | None = None,
        timeout: float | None = None,
    ) -> Any:
        headers: dict[str, str] = {"accept": "application/json"}
        if self.authorization:
            headers["authorization"] = self.authorization
        if self.runtime_token:
            headers["x-qwenpaw-runtime-token"] = self.runtime_token
        if body is not None:
            data = json.dumps(body, ensure_ascii=False).encode("utf-8")
            headers["content-type"] = "application/json"
        elif content_type:
            headers["content-type"] = content_type
        try:
            status, raw = self._transport(
                method,
                self._url(path, query),
                headers,
                data,
                timeout or self.timeout,
            )
        except HostError as exc:
            fallback = self._fallback_base_url
            if (
                not fallback
                or fallback == self.base_url
                or not _is_connection_refused(exc)
            ):
                raise
            # In Docker the browser-facing base URL is the host's published
            # port (e.g. 127.0.0.1:49154) while the container listens on 8088.
            # A refused connection had no side effect, so retrying locally is safe.
            status, raw = self._transport(
                method,
                self._url(path, query, base=fallback),
                headers,
                data,
                timeout or self.timeout,
            )
            self.base_url = fallback
            self._fallback_base_url = None
        return self._decode(status, raw)

    @staticmethod
    def _decode(status: int, raw: bytes) -> Any:
        payload: Any = {}
        if raw:
            try:
                payload = json.loads(raw.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                payload = {}
        if 200 <= status < 300:
            return payload
        detail = payload.get("detail") if isinstance(payload, dict) else None
        if isinstance(detail, dict):
            code = str(detail.get("code") or f"http_{status}")
            message = str(detail.get("message") or detail.get("msg") or detail)
        elif detail is not None:
            code = f"http_{status}"
            message = str(detail)
        else:
            code = f"http_{status}"
            message = f"宿主 API 返回 {status}"
        raise HostError(status, code, message)

    # -------------------------------------------------------------- agents ---

    def list_agents(self) -> list[dict[str, Any]]:
        body = self._request("GET", "/api/agents")
        if isinstance(body, dict):
            items = body.get("agents")
            if isinstance(items, list):
                return [item for item in items if isinstance(item, dict)]
        return body if isinstance(body, list) else []

    def get_agent(self, agent_id: str) -> dict[str, Any]:
        body = self._request("GET", f"/api/agents/{urllib.parse.quote(agent_id)}")
        if not isinstance(body, dict):
            raise HostError(0, "bad_host_response", "宿主返回的智能体配置不是对象")
        return body

    def create_agent(self, payload: dict[str, Any]) -> dict[str, Any]:
        body = self._request("POST", "/api/agents", body=payload)
        return body if isinstance(body, dict) else {}

    def delete_agent(self, agent_id: str) -> Any:
        return self._request("DELETE", f"/api/agents/{urllib.parse.quote(agent_id)}")

    def update_agent(self, agent_id: str, patch: dict[str, Any]) -> Any:
        return self._request(
            "PUT", f"/api/agents/{urllib.parse.quote(agent_id)}", body=patch
        )

    def list_workspace_files(self, agent_id: str) -> Any:
        return self._request(
            "GET", f"/api/agents/{urllib.parse.quote(agent_id)}/workspace/files"
        )

    def write_workspace_file(self, agent_id: str, name: str, content: str) -> Any:
        return self._request(
            "PUT",
            f"/api/agents/{urllib.parse.quote(agent_id)}/workspace/files/{urllib.parse.quote(name)}",
            body={"content": content},
        )

    def list_skills(self, agent_id: str) -> list[dict[str, Any]]:
        body = self._request(
            "GET", f"/api/agents/{urllib.parse.quote(agent_id)}/skills"
        )
        return body if isinstance(body, list) else []

    def upload_skills_zip(self, agent_id: str, data: bytes) -> Any:
        body, content_type = _multipart("file", "skills.zip", "application/zip", data)
        return self._request(
            "POST",
            f"/api/agents/{urllib.parse.quote(agent_id)}/skills/upload",
            query={"enable": "true"},
            data=body,
            content_type=content_type,
            timeout=max(self.timeout, 120.0),
        )

    def list_tools(self, agent_id: str) -> list[dict[str, Any]]:
        body = self._request("GET", f"/api/agents/{urllib.parse.quote(agent_id)}/tools")
        return body if isinstance(body, list) else []

    def toggle_tool(self, agent_id: str, tool_name: str) -> Any:
        return self._request(
            "PATCH",
            f"/api/agents/{urllib.parse.quote(agent_id)}/tools/{urllib.parse.quote(tool_name)}/toggle",
        )
