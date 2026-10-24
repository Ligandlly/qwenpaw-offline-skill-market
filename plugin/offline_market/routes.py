# -*- coding: utf-8 -*-
"""Plugin HTTP routes consumed by our own sidebar page (M3).

Mounted by the host at ``/api/offline-market`` (``PluginApi.register_http_router``).
The browser never talks to the market backend directly: the tenant identity
only exists inside this runtime, so every call is proxied from here.
"""

from __future__ import annotations

from typing import Any

from .client import BackendClient, BackendError
from .runtime import PluginSettings


def _status_for(error: BackendError) -> int:
    if error.status == 0:
        return 502
    return error.status if 400 <= error.status < 600 else 502


def build_router(settings: PluginSettings):
    """Import fastapi lazily: importing this module must not require it."""
    from fastapi import APIRouter, Body, HTTPException

    router = APIRouter()

    def client() -> BackendClient:
        url, tenant = settings.market_url, settings.tenant
        if not url or not tenant:
            raise HTTPException(
                status_code=503,
                detail={
                    "code": "not_configured",
                    "message": "；".join(settings.problems()) or "离线技能市场未配置",
                },
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

    @router.get("/config")
    def get_config() -> dict[str, Any]:
        gate = settings.scan_gate()
        return {
            "market_url": settings.market_url,
            "tenant": settings.tenant,
            "standalone_tenant": settings.standalone_tenant,
            "scan_mode": gate.mode,
            "scan_mode_ok": gate.ok,
            "problems": settings.problems(),
            "install_hint": "安装由 QwenPaw 宿主完成：POST /api/skills/hub/install/start",
        }

    @router.get("/limits")
    def get_limits() -> Any:
        return call(client().limits)

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

    @router.post("/skills")
    def publish(payload: dict = Body(...)) -> Any:
        files = payload.get("files")
        if not isinstance(files, dict) or not all(
            isinstance(k, str) and isinstance(v, str) for k, v in files.items()
        ):
            raise HTTPException(
                status_code=400,
                detail={"code": "invalid_files", "message": "files 必须是 {路径: 文本内容}"},
            )
        for required in ("name", "version"):
            if not str(payload.get(required) or "").strip():
                raise HTTPException(
                    status_code=400,
                    detail={"code": "invalid_publish", "message": f"缺少字段 {required}"},
                )
        return call(
            client().publish,
            name=str(payload["name"]),
            slug=payload.get("slug") or None,
            version=str(payload["version"]),
            visibility=str(payload.get("visibility") or "public"),
            files=files,
        )

    @router.post("/skills/{slug}/withdraw")
    def withdraw(slug: str, payload: dict | None = Body(default=None)) -> Any:
        reason = (payload or {}).get("reason")
        return call(client().withdraw, slug, reason)

    @router.post("/skills/{slug}/restore")
    def restore(slug: str) -> Any:
        return call(client().restore, slug)

    return router
