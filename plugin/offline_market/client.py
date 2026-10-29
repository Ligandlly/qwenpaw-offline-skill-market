# -*- coding: utf-8 -*-
"""Thin client for the offline-market backend.

stdlib only (urllib): plugins run inside the host's venv and must not add
dependencies to it. The transport is injectable, so tests never touch the
network.
"""

from __future__ import annotations

import http.client
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
    try:
        request = urllib.request.Request(url, data=body, headers=headers, method=method)
        with urllib.request.urlopen(request, timeout=timeout) as response:  # noqa: S310
            return response.status, response.read()
    except urllib.error.HTTPError as exc:  # 4xx/5xx still carry a JSON body
        return exc.code, exc.read()
    except urllib.error.URLError as exc:  # connect refused / DNS / connect timeout
        raise BackendError(0, "unreachable", f"市场后端不可达（{url}）：{exc.reason}") from exc
    except TimeoutError as exc:  # read timeout: urllib does not wrap it
        raise BackendError(0, "timeout", f"市场后端超时（{url}）：{exc}") from exc
    except (http.client.InvalidURL, ValueError) as exc:
        # Not a network failure: the configured address itself is unusable,
        # e.g. a space in the host. Map it instead of leaking a 500.
        raise BackendError(
            0, "invalid_market_url", f"市场地址无效（{url}）：{exc}"
        ) from exc
    except OSError as exc:
        raise BackendError(0, "unreachable", f"市场后端不可达（{url}）：{exc}") from exc


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

    def stats(self) -> Any:
        """Dashboard aggregates: totals, this month's new items, downloads."""
        return self._request("GET", "/api/v1/stats")

    def list_skills(
        self, query: str = "", page: int = 1, limit: int = 20, sort: str = "created"
    ) -> Any:
        return self._request(
            "GET",
            "/api/v1/skills",
            query={"q": query, "page": page, "limit": limit, "sort": sort},
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

    def derive_slug(self, name: str) -> Any:
        """Identifier a publish would derive from ``name`` (Chinese -> pinyin).

        The backend owns the rule, so the publish form prefills 标识符 with the
        exactly-same value the market would pick on its own.
        """
        return self._request("GET", "/api/v1/slug", query={"name": name})

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
        publisher_name: str | None = None,
    ) -> Any:
        body: dict[str, Any] = {
            "name": name,
            "slug": slug,
            "version": version,
            "visibility": visibility,
            "files": files,
        }
        if publisher_name:
            # Sent only when we have one: the market otherwise keeps whatever
            # name it already recorded (a publish must not blank it).
            body["publisher_name"] = publisher_name
        return self._request("POST", "/api/v1/skills", body=body)

    def validate(
        self,
        *,
        name: str,
        version: str,
        files: dict[str, Any],
        slug: str | None = None,
        visibility: str = "public",
    ) -> Any:
        """Dry run of :meth:`publish` — the publish form's 校验 step.

        Returns every problem (bundle gate, slug rules, version collision,
        ownership) plus the parsed manifest, file sizes and the suggested next
        patch version, so the page can show all of it before anything is
        uploaded. Writes nothing on the market.
        """
        body: dict[str, Any] = {
            "name": name,
            "slug": slug,
            "version": version,
            "visibility": visibility,
            "files": files,
        }
        return self._request("POST", "/api/v1/skills/validate", body=body)

    def withdraw(self, slug: str, reason: str | None = None) -> Any:
        return self._request(
            "POST",
            f"/api/v1/skills/{urllib.parse.quote(slug)}/withdraw",
            body={"reason": reason} if reason else {},
        )

    def restore(self, slug: str) -> Any:
        return self._request("POST", f"/api/v1/skills/{urllib.parse.quote(slug)}/restore")

    # ------------------------------------------------------- agent market ---

    def list_agents(
        self, query: str = "", page: int = 1, limit: int = 20, sort: str = "created"
    ) -> Any:
        return self._request(
            "GET",
            "/api/v1/agents",
            query={"q": query, "page": page, "limit": limit, "sort": sort},
        )

    def mine_agents(self) -> Any:
        return self._request("GET", "/api/v1/agents/mine")

    def agent_status(self, slug: str) -> Any:
        return self._request("GET", f"/api/v1/agents/{urllib.parse.quote(slug)}/status")

    def fetch_agent_bundle(self, slug: str, version: str) -> Any:
        return self._request(
            "GET",
            f"/api/v1/agents/{urllib.parse.quote(slug)}/"
            f"{urllib.parse.quote(version or 'latest')}/bundle",
        )

    def publish_agent(
        self,
        *,
        version: str,
        files: dict[str, Any],
        slug: str | None = None,
        visibility: str = "public",
        publisher_name: str | None = None,
    ) -> Any:
        body: dict[str, Any] = {
            "version": version,
            "visibility": visibility,
            "files": files,
        }
        if slug:
            body["slug"] = slug
        if publisher_name:
            body["publisher_name"] = publisher_name
        return self._request("POST", "/api/v1/agents", body=body)

    def withdraw_agent(self, slug: str, reason: str | None = None) -> Any:
        return self._request(
            "POST",
            f"/api/v1/agents/{urllib.parse.quote(slug)}/withdraw",
            body={"reason": reason} if reason else {},
        )

    def restore_agent(self, slug: str) -> Any:
        return self._request(
            "POST", f"/api/v1/agents/{urllib.parse.quote(slug)}/restore"
        )
