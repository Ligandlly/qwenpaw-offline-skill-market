# -*- coding: utf-8 -*-
"""Plugin HTTP routes consumed by our own sidebar page (M3).

Mounted by the host at ``/api/offline-market`` (``PluginApi.register_http_router``).
The browser never talks to the market backend directly: the tenant identity
only exists inside this runtime, so every call is proxied from here.
"""

from __future__ import annotations

import os
from typing import Any, NoReturn

from .agents import (
    AgentError,
    AgentInstaller,
    InstalledLedger,
    bundle_summary,
    installed_items,
    snapshot_agent,
    uninstall_agent,
)
from .bundle import BundleError, extract_zip, wire_files
from .client import BackendClient, BackendError
from .host import HostClient, HostError, default_local_base_url, resolve_base_url
from .runtime import PluginSettings


def build_installer(settings: PluginSettings, host: HostClient) -> AgentInstaller:
    """Kept module-level so route tests can substitute the whole state machine."""
    return AgentInstaller(
        host,
        InstalledLedger.from_settings(settings),
        ready_timeout=settings.install_ready_timeout,
        poll=settings.install_poll,
    )



def _status_for(error: BackendError) -> int:
    if error.status == 0:
        return 502
    return error.status if 400 <= error.status < 600 else 502

# Publisher names are labels, never identities: ownership stays the tenant header
# the runtime sets, so a page can only mislabel itself. Clamped anyway — the
# market stores this next to the tenant row.
MAX_PUBLISHER_NAME = 64

def clean_publisher_name(raw: Any) -> str | None:
    """Trim and clamp a browser-supplied publisher name; ``None`` if unusable."""
    if not isinstance(raw, str):
        return None
    cleaned = "".join(char for char in raw if char.isprintable()).strip()
    return cleaned[:MAX_PUBLISHER_NAME] or None


def build_router(settings: PluginSettings):
    """Import fastapi lazily: importing this module must not require it."""
    from fastapi import APIRouter, Body, HTTPException, Request

    # `from __future__ import annotations` stores "Request" as a string, and
    # FastAPI resolves annotations from *module* globals. Publish the lazily
    # imported class there so Request injection works without importing
    # fastapi at module import time.
    globals().setdefault("Request", Request)

    router = APIRouter()

    def client() -> BackendClient:
        url, tenant = settings.market_url, settings.tenant
        if not tenant:
            raise HTTPException(
                status_code=503,
                detail={
                    "code": "not_configured",
                    "message": "；".join(settings.problems()) or "离线技能市场未配置",
                },
            )
        url_problem = settings.market_url_problem()
        if url_problem:
            raise HTTPException(
                status_code=503,
                detail={"code": "invalid_market_url", "message": url_problem},
            )
        return BackendClient(url, tenant, timeout=settings.timeout)

    def call(fn, *args, **kwargs) -> Any:
        try:
            return fn(*args, **kwargs)
        except HTTPException:
            raise
        except BackendError as exc:
            raise HTTPException(
                status_code=_status_for(exc),
                detail={"code": exc.code, "message": str(exc), "backend_status": exc.status},
            ) from exc

    def host_client(request: Request) -> HostClient:
        request_base = str(request.base_url).rstrip("/")
        base = resolve_base_url(settings, request_base_url=request_base)
        # Only fall back when this base was inferred from the browser. Explicit
        # config/env must fail loudly instead of being silently overridden.
        fallback = None
        if base != resolve_base_url(settings):
            fallback = default_local_base_url()
        return HostClient(
            base,
            authorization=request.headers.get("authorization"),
            runtime_token=os.environ.get("QWENPAW_RUNTIME_INTERNAL_TOKEN"),
            timeout=settings.host_timeout,
            fallback_base_url=fallback,
        )

    def raise_agent_http(exc: Exception) -> NoReturn:
        if isinstance(exc, AgentError):
            status = exc.status if 400 <= exc.status < 600 else 502
            detail = exc.as_detail()
        else:
            assert isinstance(exc, HostError)
            status = exc.status if 400 <= exc.status < 600 else 502
            detail = {"code": exc.code, "message": str(exc)}
        raise HTTPException(status_code=status, detail=detail)

    @router.get("/config")
    def get_config(request: Request) -> dict[str, Any]:
        gate = settings.scan_gate()
        host_api_url: str | None = None
        host_api_error: str | None = None
        try:
            host_api_url = host_client(request).base_url
        except HostError as exc:
            host_api_error = str(exc)
        return {
            "market_url": settings.market_url,
            "tenant": settings.tenant,
            "standalone_tenant": settings.standalone_tenant,
            "scan_mode": gate.mode,
            "scan_mode_ok": gate.ok,
            "host_api_url": host_api_url,
            "host_api_error": host_api_error,
            "runtime_token_present": bool(
                os.environ.get("QWENPAW_RUNTIME_INTERNAL_TOKEN")
            ),
            "problems": settings.problems(),
            "install_hint": "安装由 QwenPaw 宿主完成：POST /api/skills/hub/install/start",
        }

    @router.get("/host-health")
    def host_health(request: Request) -> dict[str, Any]:
        """Diagnostics for Docker/Hub deployments: can this runtime call itself?

        Always returns 200 so a preflight can print the reason instead of a bare
        HTTP error; `ok` is the signal.
        """
        try:
            host = host_client(request)
        except HostError as exc:
            return {"ok": False, "code": exc.code, "message": str(exc)}
        try:
            agents = host.list_agents()
        except HostError as exc:
            return {
                "ok": False,
                "host_api_url": getattr(host, "base_url", None),
                "code": exc.code,
                "message": str(exc),
            }
        return {
            "ok": True,
            "host_api_url": host.base_url,
            "agent_count": len(agents),
            "runtime_token_present": bool(host.runtime_token),
        }

    @router.get("/limits")
    def get_limits() -> Any:
        return call(client().limits)

    @router.get("/stats")
    def get_stats() -> Any:
        return call(client().stats)

    @router.get("/skills")
    def list_skills(q: str = "", page: int = 1, limit: int = 20) -> Any:
        return call(client().list_skills, q, page, limit)

    @router.get("/mine")
    def list_mine() -> Any:
        return call(client().mine)

    @router.get("/lookup")
    def lookup(names: str = "") -> Any:
        wanted = [name.strip() for name in names.split(",") if name.strip()]
        if not wanted:
            return {"items": []}
        return call(client().lookup, wanted)

    @router.get("/skills/{slug}/status")
    def skill_status(slug: str) -> Any:
        return call(client().status, slug)

    def required(name: str, version: str) -> tuple[str, str]:
        for field, value in (("name", name), ("version", version)):
            if not value.strip():
                raise HTTPException(
                    status_code=400,
                    detail={"code": "invalid_publish", "message": f"缺少字段 {field}"},
                )
        return name.strip(), version.strip()

    def valid_file(value: Any) -> bool:
        """Text is a plain string; binary is `{"base64": "..."}` (see bundle.py)."""
        if isinstance(value, str):
            return True
        return (
            isinstance(value, dict)
            and set(value) == {"base64"}
            and isinstance(value["base64"], str)
        )

    @router.post("/skills")
    def publish(payload: dict = Body(...)) -> Any:
        files = payload.get("files")
        if not isinstance(files, dict) or not all(
            isinstance(k, str) and valid_file(v) for k, v in files.items()
        ):
            raise HTTPException(
                status_code=400,
                detail={
                    "code": "invalid_files",
                    "message": "files 必须是 {路径: 文本} 或 {路径: {'base64': ...}}",
                },
            )
        name, version = required(
            str(payload.get("name") or ""), str(payload.get("version") or "")
        )
        return call(
            client().publish,
            name=name,
            slug=payload.get("slug") or None,
            version=version,
            visibility=str(payload.get("visibility") or "public"),
            publisher_name=clean_publisher_name(payload.get("publisher_name")),
            files=files,
        )

    @router.post("/skills/zip")
    def publish_zip(
        payload: bytes = Body(..., media_type="application/zip"),
        name: str = "",
        slug: str = "",
        version: str = "",
        visibility: str = "public",
        publisher_name: str = "",
    ) -> Any:
        """Publish from a .zip upload: the page sends the archive as raw body.

        Raw body instead of multipart keeps the plugin free of the
        python-multipart dependency (plugins run inside the host's venv).
        """
        name, version = required(name, version)
        try:
            files = wire_files(extract_zip(payload))
        except BundleError as exc:
            raise HTTPException(
                status_code=400,
                detail={"code": exc.code, "message": str(exc)},
            ) from exc
        return call(
            client().publish,
            name=name,
            slug=slug.strip() or None,
            version=version,
            visibility=visibility.strip() or "public",
            publisher_name=clean_publisher_name(publisher_name),
            files=files,
        )

    @router.post("/skills/{slug}/withdraw")
    def withdraw(slug: str, payload: dict | None = Body(default=None)) -> Any:
        reason = (payload or {}).get("reason")
        return call(client().withdraw, slug, reason)

    @router.post("/skills/{slug}/restore")
    def restore(slug: str) -> Any:
        return call(client().restore, slug)

    # ------------------------------------------------------- agent market ---

    @router.get("/agents")
    def list_agents(q: str = "", page: int = 1, limit: int = 20) -> Any:
        return call(client().list_agents, q, page, limit)

    @router.get("/agents/mine")
    def list_my_agents() -> Any:
        return call(client().mine_agents)

    @router.get("/agents/installed")
    def installed_agents(request: Request) -> Any:
        try:
            host = host_client(request)
        except HostError as exc:
            raise_agent_http(exc)
        backend = client()
        return {
            "items": installed_items(
                host,
                InstalledLedger.from_settings(settings),
                backend.agent_status,
            )
        }

    @router.get("/agents/{slug}/status")
    def agent_status(slug: str) -> Any:
        return call(client().agent_status, slug)

    @router.get("/host-agents")
    def list_host_agents(request: Request) -> Any:
        try:
            agents = host_client(request).list_agents()
        except HostError as exc:
            raise_agent_http(exc)
        items = []
        for agent in agents:
            if str(agent.get("backend") or "qwenpaw") != "qwenpaw":
                continue
            items.append(
                {
                    "id": agent.get("id"),
                    "name": agent.get("name"),
                    "description": agent.get("description"),
                    "workspace_dir": agent.get("workspace_dir"),
                    "enabled": agent.get("enabled", True),
                    "startup_status": agent.get("startup_status"),
                }
            )
        return {"items": items}

    @router.post("/agents/publish")
    def publish_agent(request: Request, payload: dict = Body(...)) -> Any:
        source = str(payload.get("source_agent_id") or "").strip()
        version = str(payload.get("version") or "").strip()
        if not source or not version:
            raise HTTPException(
                status_code=400,
                detail={
                    "code": "invalid_publish",
                    "message": "缺少 source_agent_id 或 version",
                },
            )
        try:
            snapshot = snapshot_agent(host_client(request), source)
        except (HostError, AgentError) as exc:
            raise_agent_http(exc)

        slug = str(payload.get("slug") or "").strip() or None
        return call(
            client().publish_agent,
            slug=slug,
            version=version,
            files=snapshot["files"],
            visibility=str(payload.get("visibility") or "public"),
            publisher_name=clean_publisher_name(payload.get("publisher_name")),
        )

    @router.post("/agents/preview")
    def preview_agent(request: Request, payload: dict = Body(...)) -> Any:
        source = str(payload.get("source_agent_id") or "").strip()
        if not source:
            raise HTTPException(
                status_code=400,
                detail={"code": "invalid_preview", "message": "缺少 source_agent_id"},
            )
        try:
            snapshot = snapshot_agent(host_client(request), source)
        except (HostError, AgentError) as exc:
            raise_agent_http(exc)
        manifest = snapshot["manifest"]
        return {
            "manifest": manifest,
            "md_files": manifest.get("md_files") or [],
            "skills": manifest.get("skills") or [],
            **bundle_summary(snapshot["files"]),
        }

    @router.post("/agents/{slug}/withdraw")
    def withdraw_agent(slug: str, payload: dict | None = Body(default=None)) -> Any:
        reason = (payload or {}).get("reason")
        return call(client().withdraw_agent, slug, reason)

    @router.post("/agents/{slug}/restore")
    def restore_agent(slug: str) -> Any:
        return call(client().restore_agent, slug)

    @router.post("/agents/{slug}/install")
    def install_agent(
        slug: str,
        request: Request,
        payload: dict | None = Body(default=None),
    ) -> Any:
        body = payload or {}
        version = str(body.get("version") or "latest").strip() or "latest"
        target = body.get("target_name")
        target_name = (
            str(target).strip()[:128]
            if isinstance(target, str) and target.strip()
            else None
        )
        bundle = call(client().fetch_agent_bundle, slug, version)
        try:
            installer = build_installer(settings, host_client(request))
            return installer.install(
                slug=slug, bundle=bundle, target_name=target_name
            )
        except (HostError, AgentError) as exc:
            raise_agent_http(exc)

    @router.post("/agents/{agent_id}/uninstall")
    def uninstall(agent_id: str, request: Request) -> Any:
        try:
            return uninstall_agent(
                host_client(request),
                InstalledLedger.from_settings(settings),
                agent_id,
            )
        except (HostError, AgentError) as exc:
            raise_agent_http(exc)

    return router
