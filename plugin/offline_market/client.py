# -*- coding: utf-8 -*-
"""Thin client for the offline-market backend.

stdlib only (urllib): plugins run inside the host's venv and must not add
dependencies to it. The transport is injectable, so tests never touch the
network.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Callable

# (method, url, headers, body-or-None, timeout) -> (status, raw body)
Transport = Callable[[str, str, dict[str, str], bytes | None, float], tuple[int, bytes]]


class BackendError(RuntimeError):
    """The backend answered with something we cannot use."""

    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.code = code

    def __str__(self) -> str:  # host shows this verbatim in some paths
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
        raise BackendError(0, "unreachable", f"市场后端不可达：{exc.reason}") from exc


class BackendClient:
    def __init__(
        self,
        base_url: str,
        tenant: str,
        *,
        timeout: float = 10.0,
        transport: Transport | None = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.tenant = tenant
        self.timeout = timeout
        self._transport = transport or urllib_transport

    # ---------------------------------------------------------- plumbing ---

    def _url(self, path: str, query: dict[str, Any] | None = None) -> str:
        url = f"{self.base_url}{path}"
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
        timeout: float | None = None,
    ) -> Any:
        headers = {"accept": "application/json", "x-tenant-id": self.tenant}
        data = None
        if body is not None:
            data = json.dumps(body, ensure_ascii=False).encode("utf-8")
            headers["content-type"] = "application/json"
        status, raw = self._transport(
            method, self._url(path, query), headers, data, timeout or self.timeout
        )
        return self._decode(status, raw)

    @staticmethod
    def _decode(status: int, raw: bytes) -> Any:
        try:
            payload = json.loads(raw.decode("utf-8")) if raw else {}
        except (UnicodeDecodeError, json.JSONDecodeError):
            payload = {}
        if 200 <= status < 300:
            return payload
        error = payload.get("error") if isinstance(payload, dict) else None
        code = (error or {}).get("code") or f"http_{status}"
        message = (error or {}).get("message") or f"市场后端返回 {status}"
        raise BackendError(status, code, message)

    # ------------------------------------------------------------ market ---

    def health(self, timeout: float | None = None) -> Any:
        return self._request("GET", "/health", timeout=timeout)

    def limits(self) -> Any:
        return self._request("GET", "/api/v1/limits")

    def list_skills(self, query: str = "", page: int = 1, limit: int = 20) -> Any:
        return self._request(
            "GET", "/api/v1/skills", query={"q": query, "page": page, "limit": limit}
        )

    def mine(self) -> Any:
        return self._request("GET", "/api/v1/skills/mine")

    def lookup(self, names: list[str]) -> Any:
        """display name -> slug + state, including withdrawn/taken-down skills."""
        return self._request(
            "GET", "/api/v1/skills/lookup", query={"names": ",".join(names)}
        )

    def status(self, slug: str) -> Any:
        return self._request("GET", f"/api/v1/skills/{urllib.parse.quote(slug)}/status")

    def fetch_bundle(self, slug: str, version: str) -> Any:
        return self._request(
            "GET", f"/skills/{urllib.parse.quote(slug)}/{urllib.parse.quote(version or 'latest')}"
        )

    # ----------------------------------------------------------- publish ---

    def publish(
        self,
        *,
        name: str,
        version: str,
        files: dict[str, str],
        slug: str | None = None,
        visibility: str = "public",
    ) -> Any:
        return self._request(
            "POST",
            "/api/v1/skills",
            body={
                "name": name,
                "slug": slug,
                "version": version,
                "visibility": visibility,
                "files": files,
            },
        )

    def withdraw(self, slug: str, reason: str | None = None) -> Any:
        return self._request(
            "POST",
            f"/api/v1/skills/{urllib.parse.quote(slug)}/withdraw",
            body={"reason": reason} if reason else {},
        )

    def restore(self, slug: str) -> Any:
        return self._request("POST", f"/api/v1/skills/{urllib.parse.quote(slug)}/restore")
