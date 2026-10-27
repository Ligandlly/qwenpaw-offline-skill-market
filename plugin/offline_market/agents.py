# -*- coding: utf-8 -*-
"""Agent bundle snapshot + installation state machine.

Publishing snapshots an existing local agent (read-only) into
``manifest.json`` + persona markdown + ``skills/<name>/**``. Installing does the
opposite over the host's own HTTP API: create agent, wait for its workspace,
write markdown, upload bundled skills (the host scans them), apply the safe
config subset, then record the install locally.

No quotas and no remote update in the MVP: see docs/AGENT_MARKET.md.
"""

from __future__ import annotations

import base64
import io
import json
import os
import threading
import time
import zipfile
from pathlib import Path
from typing import Any, Callable

from .host import HostClient, HostError

AGENT_MANIFEST = "manifest.json"
AGENT_MD_FILES = (
    "AGENTS.md",
    "SOUL.md",
    "PROFILE.md",
    "HEARTBEAT.md",
    "BOOTSTRAP.md",
)
IGNORED_DIRS = frozenset(
    {".git", "__pycache__", ".pytest_cache", ".venv", "node_modules"}
)
IGNORED_FILES = frozenset({".DS_Store", "Thumbs.db", "desktop.ini"})
MAX_AGENT_ID = 64

_LEDGER_VERSION = 1
_LOCKS: dict[str, threading.Lock] = {}
_LOCKS_GUARD = threading.Lock()
_LEDGER_LOCK = threading.Lock()


class AgentError(RuntimeError):
    """A user-actionable failure with enough structure for the page."""

    def __init__(
        self,
        code: str,
        message: str,
        *,
        status: int = 0,
        step: str | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.status = status
        self.step = step
        self.rollback_failed = False
        self.workspace_kept: str | None = None

    def as_detail(self) -> dict[str, Any]:
        detail: dict[str, Any] = {"code": self.code, "message": str(self)}
        if self.step:
            detail["step"] = self.step
        if self.rollback_failed:
            detail["rollback_failed"] = True
        if self.workspace_kept:
            detail["workspace_kept"] = self.workspace_kept
        return detail


# ------------------------------------------------------------------ ledger ---


class InstalledLedger:
    """Which agents this runtime installed from the market.

    Host ``template_id`` is deliberately not used as a marker: agent.json is
    host-owned, and a local ledger cannot leak into a published bundle.
    """

    def __init__(self, path: str | Path) -> None:
        self.path = Path(path)

    @classmethod
    def from_settings(cls, settings) -> "InstalledLedger":
        return cls(settings.state_dir / "installed-agents.json")

    def _load(self) -> dict[str, Any]:
        try:
            raw = json.loads(self.path.read_text(encoding="utf-8"))
        except (FileNotFoundError, OSError, json.JSONDecodeError):
            return {"version": _LEDGER_VERSION, "items": {}}
        if not isinstance(raw, dict) or not isinstance(raw.get("items"), dict):
            return {"version": _LEDGER_VERSION, "items": {}}
        return raw

    def _write(self, payload: dict[str, Any]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_name(f".{self.path.name}.{os.getpid()}.tmp")
        tmp.write_text(
            json.dumps(payload, ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
        os.replace(tmp, self.path)

    def items(self) -> list[dict[str, Any]]:
        raw = self._load().get("items", {})
        items = [dict(value) for value in raw.values() if isinstance(value, dict)]
        items.sort(key=lambda item: int(item.get("installed_at") or 0), reverse=True)
        return items

    def get(self, agent_id: str) -> dict[str, Any] | None:
        value = self._load().get("items", {}).get(agent_id)
        return dict(value) if isinstance(value, dict) else None

    def find_by_slug(self, slug: str) -> dict[str, Any] | None:
        for item in self.items():
            if item.get("slug") == slug:
                return item
        return None

    def add(self, entry: dict[str, Any]) -> None:
        with _LEDGER_LOCK:
            payload = self._load()
            payload.setdefault("items", {})[str(entry["agent_id"])] = entry
            payload["version"] = _LEDGER_VERSION
            self._write(payload)

    def remove(self, agent_id: str) -> None:
        with _LEDGER_LOCK:
            payload = self._load()
            items = payload.setdefault("items", {})
            if items.pop(str(agent_id), None) is not None:
                self._write(payload)

    def reconcile(self, existing_ids: set[str]) -> bool:
        with _LEDGER_LOCK:
            payload = self._load()
            items = payload.setdefault("items", {})
            stale = [key for key in items if key not in existing_ids]
            if not stale:
                return False
            for key in stale:
                items.pop(key, None)
            self._write(payload)
            return True


# ---------------------------------------------------------------- snapshot ---


def _safe_relpath(path: str) -> bool:
    if not path or len(path) > 300 or "\\" in path or path.startswith("/"):
        return False
    if ":" in path.split("/", 1)[0]:
        return False
    if any(ord(char) < 32 for char in path):
        return False
    return all(segment not in {"", ".", ".."} for segment in path.split("/"))


def _wire_value(raw: bytes) -> str | dict[str, str]:
    try:
        return raw.decode("utf-8-sig")
    except UnicodeDecodeError:
        return {"base64": base64.b64encode(raw).decode("ascii")}


def _read_markdown(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8-sig")
    except UnicodeDecodeError as exc:
        raise AgentError(
            "invalid_markdown",
            f"{path.name} 不是 UTF-8 文本，无法发布（人设文件必须是文本）",
            status=400,
        ) from exc
    except OSError as exc:
        raise AgentError(
            "unreadable_markdown", f"读不到 {path.name}：{exc}", status=400
        ) from exc


def _skills_dir(workspace: Path) -> Path:
    preferred = workspace / "skills"
    if preferred.is_dir():
        return preferred
    legacy = workspace / "skill"
    if legacy.is_dir():
        return legacy
    return preferred


def snapshot_agent(host: HostClient, agent_id: str) -> dict[str, Any]:
    """Read one local agent into `{manifest, files}` without mutating it."""
    config = host.get_agent(agent_id)
    backend = str(config.get("backend") or "qwenpaw")
    if backend != "qwenpaw":
        raise AgentError(
            "unsupported_backend",
            f"只能发布 qwenpaw 后端的智能体（当前 {backend}）",
            status=400,
        )
    workspace = Path(str(config.get("workspace_dir") or "")).expanduser()
    if not workspace.is_dir():
        raise AgentError(
            "workspace_missing",
            f"智能体工作区不存在：{workspace}",
            status=400,
        )

    files: dict[str, Any] = {}
    md_files: list[str] = []
    for name in AGENT_MD_FILES:
        path = workspace / name
        if not path.is_file() or path.is_symlink():
            continue
        files[name] = _read_markdown(path)
        md_files.append(name)

    skill_names: list[str] = []
    root = _skills_dir(workspace)
    for item in host.list_skills(agent_id):
        name = str(item.get("name") or "").strip()
        if not name or item.get("enabled") is not True:
            continue
        if not _valid_skill_name(name):
            raise AgentError(
                "invalid_skill_name",
                f"技能名 {name!r} 不合法（不能为空、不能含路径分隔符/控制字符，且不超过 128 字节）",
                status=400,
            )
        skill_dir = root / name
        if not skill_dir.is_dir() or skill_dir.is_symlink():
            raise AgentError(
                "skill_dir_missing",
                f"宿主报告技能 {name} 已启用，但目录不存在：{skill_dir}",
                status=400,
            )
        collected = 0
        for current, dirs, names in os.walk(skill_dir, followlinks=False):
            dirs[:] = [
                name
                for name in dirs
                if name not in IGNORED_DIRS
                and not (Path(current) / name).is_symlink()
            ]
            for filename in names:
                if filename in IGNORED_FILES:
                    continue
                path = Path(current) / filename
                if path.is_symlink() or not path.is_file():
                    continue
                rel = path.relative_to(skill_dir).as_posix()
                key = f"skills/{name}/{rel}"
                if not _safe_relpath(key):
                    raise AgentError(
                        "invalid_path", f"技能文件路径不合法：{key}", status=400
                    )
                try:
                    raw = path.read_bytes()
                except OSError as exc:
                    raise AgentError(
                        "unreadable_skill_file",
                        f"读不到技能文件 {key}：{exc}",
                        status=400,
                    ) from exc
                files[key] = _wire_value(raw)
                collected += 1
        if f"skills/{name}/SKILL.md" not in files or not files.get(
            f"skills/{name}/SKILL.md"
        ):
            raise AgentError(
                "missing_skill_manifest",
                f"技能 {name} 缺少 SKILL.md",
                status=400,
            )
        if collected == 0:  # pragma: no cover - SKILL.md would already fail
            raise AgentError(
                "empty_skill", f"技能 {name} 没有可发布的文件", status=400
            )
        skill_names.append(name)

    if not md_files and not skill_names:
        raise AgentError(
            "empty_agent",
            "这个智能体既没有人设 MD，也没有启用的技能，无法发布",
            status=400,
        )

    profile: dict[str, Any] = {}
    level = config.get("thinking_level")
    if isinstance(level, str) and level:
        profile["thinking_level"] = level
    budget = config.get("thinking_budget")
    if isinstance(budget, int) and not isinstance(budget, bool):
        profile["thinking_budget"] = budget
    plan = config.get("plan")
    if isinstance(plan, dict):
        profile["plan_enabled"] = bool(plan.get("enabled"))
    tools = config.get("tools")
    if isinstance(tools, dict):
        builtin = tools.get("builtin_tools")
        if isinstance(builtin, dict):
            profile["disabled_tools"] = sorted(
                name
                for name, value in builtin.items()
                if isinstance(value, dict) and value.get("enabled") is False
            )

    manifest = {
        "schema": 1,
        "name": str(config.get("name") or agent_id).strip() or agent_id,
        "description": str(config.get("description") or "").strip(),
        "language": str(config.get("language") or "zh"),
        "profile": profile,
        "md_files": md_files,
        "skills": skill_names,
    }
    files[AGENT_MANIFEST] = json.dumps(manifest, ensure_ascii=False, indent=2)
    return {"manifest": manifest, "files": files}


def _valid_skill_name(name: str) -> bool:
    """Mirror the host's ``normalize_skill_dir_name``.

    The host accepts every non-empty directory name without path separators or
    control characters (uppercase and non-ASCII included). Keeping the same
    rule here lets existing agents such as ``QA_source_index`` publish as-is.
    """
    if not name or name.strip() != name or name in {".", ".."}:
        return False
    if len(name.encode("utf-8")) > 128:
        return False
    if "/" in name or "\\" in name:
        return False
    return not any(ord(char) < 0x20 or 0x7F <= ord(char) <= 0x9F for char in name)


# ------------------------------------------------------------------ bundle ---


def parse_bundle(files: Any) -> dict[str, Any]:
    """Validate an incoming bundle enough that every write stays contained."""
    if not isinstance(files, dict):
        raise AgentError("bad_bundle", "市场返回的智能体包不是文件映射", status=502)
    raw = files.get(AGENT_MANIFEST)
    if not isinstance(raw, str):
        raise AgentError("bad_bundle", "智能体包缺少 manifest.json", status=502)
    try:
        manifest = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise AgentError("bad_bundle", f"manifest.json 解析失败：{exc}", status=502) from exc
    if not isinstance(manifest, dict):
        raise AgentError("bad_bundle", "manifest.json 必须是对象", status=502)
    if manifest.get("schema") != 1 or not str(manifest.get("name") or "").strip():
        raise AgentError("bad_bundle", "manifest.json 的 schema/name 不合法", status=502)

    md_files = manifest.get("md_files") or []
    skills = manifest.get("skills") or []
    if not isinstance(md_files, list) or not isinstance(skills, list):
        raise AgentError("bad_bundle", "manifest.json 的 md_files/skills 必须是数组", status=502)
    md_files = [str(name) for name in md_files]
    skills = [str(name) for name in skills]
    if not md_files and not skills:
        raise AgentError("bad_bundle", "智能体包既没有人设也没有技能", status=502)

    for path in files:
        if not isinstance(path, str) or not _safe_relpath(path):
            raise AgentError("bad_bundle", f"不合法路径：{path!r}", status=502)
        if path == AGENT_MANIFEST:
            continue
        if "/" not in path:
            if path not in md_files or path not in AGENT_MD_FILES:
                raise AgentError("bad_bundle", f"根目录文件未声明：{path}", status=502)
            if not isinstance(files[path], str):
                raise AgentError("bad_bundle", f"{path} 必须是 UTF-8 文本", status=502)
            continue
        parts = path.split("/")
        if parts[0] != "skills" or len(parts) < 3 or parts[1] not in skills:
            raise AgentError("bad_bundle", f"技能文件未在 manifest 中声明：{path}", status=502)

    for name in md_files:
        if name not in AGENT_MD_FILES or name not in files:
            raise AgentError("bad_bundle", f"人设文件缺失：{name}", status=502)
    for name in skills:
        if not _valid_skill_name(name):
            raise AgentError("bad_bundle", f"技能名不合法：{name}", status=502)
        key = f"skills/{name}/SKILL.md"
        if not isinstance(files.get(key), str):
            raise AgentError("bad_bundle", f"技能缺少 SKILL.md：{name}", status=502)
    return manifest


def bundle_summary(files: dict[str, Any]) -> dict[str, int]:
    """Counts/sizes for the publish preview; never returns file contents."""
    size = 0
    for value in files.values():
        if isinstance(value, str):
            size += len(value.encode("utf-8"))
        elif isinstance(value, dict) and isinstance(value.get("base64"), str):
            try:
                size += len(base64.b64decode(value["base64"], validate=True))
            except (ValueError, TypeError):
                continue
    return {"file_count": len(files), "size_bytes": size}


def build_skills_zip(files: dict[str, Any], skills: list[str]) -> bytes | None:
    """Repackage `skills/<name>/**` as a normal skill zip for the host."""
    selected = set(skills)
    entries: list[tuple[str, bytes]] = []
    for path, value in files.items():
        if not path.startswith("skills/"):
            continue
        parts = path.split("/", 2)
        if len(parts) != 3 or parts[1] not in selected:
            continue
        if isinstance(value, str):
            raw = value.encode("utf-8")
        elif isinstance(value, dict) and isinstance(value.get("base64"), str):
            try:
                raw = base64.b64decode(value["base64"], validate=True)
            except (ValueError, TypeError) as exc:
                raise AgentError("bad_bundle", f"{path}: base64 解码失败", status=502) from exc
        else:
            raise AgentError("bad_bundle", f"{path}: 文件内容不合法", status=502)
        entries.append((f"{parts[1]}/{parts[2]}", raw))
    if not entries:
        return None
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, raw in sorted(entries):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, raw)
    return buffer.getvalue()


# ----------------------------------------------------------------- install ---


def _lock_for(slug: str) -> threading.Lock:
    with _LOCKS_GUARD:
        return _LOCKS.setdefault(slug, threading.Lock())


def _host_agent_id(slug: str, existing: set[str]) -> str:
    base = slug.strip("-_")
    if len(base) < 2:
        base = f"agent-{base}" if base else "agent"
    if base == "default":
        base = "agent-default"
    base = base[:MAX_AGENT_ID].rstrip("-_") or "agent"
    if base not in existing:
        return base
    counter = 2
    while True:
        suffix = f"-{counter}"
        candidate = f"{base[: MAX_AGENT_ID - len(suffix)].rstrip('-_')}{suffix}"
        if candidate not in existing:
            return candidate
        counter += 1


class AgentInstaller:
    def __init__(
        self,
        host: HostClient,
        ledger: InstalledLedger,
        *,
        ready_timeout: float = 60.0,
        poll: float = 0.5,
        sleep: Callable[[float], None] = time.sleep,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.host = host
        self.ledger = ledger
        self.ready_timeout = ready_timeout
        self.poll = poll
        self.sleep = sleep
        self.clock = clock

    def install(
        self,
        *,
        slug: str,
        bundle: dict[str, Any],
        target_name: str | None = None,
    ) -> dict[str, Any]:
        if not isinstance(bundle, dict):
            raise AgentError("bad_bundle", "市场返回的智能体包不是对象", status=502)
        files = bundle.get("files")
        manifest = parse_bundle(files)
        assert isinstance(files, dict)  # parse_bundle already proved it

        lock = _lock_for(slug)
        if not lock.acquire(timeout=10):
            raise AgentError(
                "install_in_progress", "该智能体正在安装，请稍候", status=409
            )
        try:
            return self._install_locked(
                slug,
                manifest,
                files,
                target_name,
                str(bundle.get("version") or ""),
            )
        finally:
            lock.release()

    def _install_locked(
        self,
        slug: str,
        manifest: dict[str, Any],
        files: dict[str, Any],
        target_name: str | None,
        version: str,
    ) -> dict[str, Any]:
        if self.ledger.find_by_slug(slug):
            raise AgentError(
                "already_installed",
                "这个智能体已经安装过了；请先卸载，或先在智能体列表里改名",
                status=409,
            )

        agents = self.host.list_agents()
        existing = {str(item.get("id") or "") for item in agents}
        agent_id = _host_agent_id(slug, existing)
        name = (target_name or "").strip() or str(manifest.get("name") or slug)
        profile = manifest.get("profile") if isinstance(manifest.get("profile"), dict) else {}

        payload: dict[str, Any] = {
            "id": agent_id,
            "name": name,
            "description": str(manifest.get("description") or ""),
            "language": str(manifest.get("language") or "zh"),
            "skill_names": [],
        }
        if isinstance(profile.get("thinking_level"), str):
            payload["thinking_level"] = profile["thinking_level"]
        if isinstance(profile.get("thinking_budget"), int):
            payload["thinking_budget"] = profile["thinking_budget"]

        created = False
        workspace_dir: str | None = None
        step = "create_agent"
        try:
            created_info: dict[str, Any] | None = None
            for attempt in range(3):
                try:
                    created_info = self.host.create_agent(dict(payload))
                    created = True
                    break
                except HostError as exc:
                    # Two installs can race between list_agents and create.
                    # The host answers 400 "already exists"; pick the next suffix.
                    if (
                        exc.status == 400
                        and "exist" in str(exc).lower()
                        and attempt < 2
                    ):
                        existing.add(agent_id)
                        agent_id = _host_agent_id(slug, existing)
                        payload["id"] = agent_id
                        continue
                    raise
            if created_info is None:  # pragma: no cover - loop raises instead
                raise AgentError(
                    "create_failed", "宿主没有返回新建的智能体", status=502
                )
            workspace_dir = str(created_info.get("workspace_dir") or "") or None

            step = "wait_ready"
            self._wait_ready(agent_id)

            step = "write_markdown"
            for md_name in manifest.get("md_files") or []:
                self.host.write_workspace_file(agent_id, md_name, files[md_name])

            step = "install_skills"
            zip_data = build_skills_zip(files, [str(s) for s in manifest.get("skills") or []])
            if zip_data:
                self.host.upload_skills_zip(agent_id, zip_data)

            step = "apply_config"
            self._apply_config(agent_id, profile)

            step = "ledger"
            warnings: list[str] = []
            entry = {
                "agent_id": agent_id,
                "slug": slug,
                "version": version,
                "name": name,
                "workspace_dir": workspace_dir,
                "installed_at": int(time.time()),
            }
            try:
                self.ledger.add(entry)
            except OSError as exc:
                warnings.append(f"安装成功，但本机安装账本写入失败：{exc}")

            return {
                "agent_id": agent_id,
                "name": name,
                "workspace_dir": workspace_dir,
                "version": entry["version"],
                "warnings": warnings,
            }
        except Exception as exc:  # noqa: BLE001 - converted/rollback below
            rollback_failed = False
            if created:
                try:
                    self.host.delete_agent(agent_id)
                except HostError:
                    rollback_failed = True
            if isinstance(exc, AgentError):
                error = exc
            elif isinstance(exc, HostError):
                error = AgentError(
                    exc.code, str(exc), status=exc.status, step=step
                )
            else:
                error = AgentError("install_failed", str(exc), step=step)
            if error.step is None:
                error.step = step
            error.rollback_failed = rollback_failed
            error.workspace_kept = workspace_dir
            raise error from exc

    def _wait_ready(self, agent_id: str) -> None:
        deadline = self.clock() + self.ready_timeout
        last: HostError | None = None
        while True:
            try:
                self.host.list_workspace_files(agent_id)
                return
            except HostError as exc:
                if exc.status in (0, 404, 409, 500):
                    last = exc
                else:
                    raise
            if self.clock() >= deadline:
                raise AgentError(
                    "agent_not_ready",
                    f"智能体已创建，但工作区在 {self.ready_timeout:g}s 内未就绪"
                    + (f"（宿主：{last}）" if last else ""),
                    status=504,
                    step="wait_ready",
                )
            self.sleep(self.poll)

    def _apply_config(self, agent_id: str, profile: dict[str, Any]) -> None:
        if profile.get("plan_enabled"):
            self.host.update_agent(agent_id, {"plan": {"enabled": True}})

        wanted = [
            str(name)
            for name in (profile.get("disabled_tools") or [])
            if isinstance(name, str) and name
        ]
        if not wanted:
            return
        current = {
            str(item.get("name")): bool(item.get("enabled"))
            for item in self.host.list_tools(agent_id)
            if isinstance(item, dict) and item.get("name")
        }
        for name in wanted:
            if current.get(name) is True:
                self.host.toggle_tool(agent_id, name)
        after = {
            str(item.get("name")): bool(item.get("enabled"))
            for item in self.host.list_tools(agent_id)
            if isinstance(item, dict) and item.get("name")
        }
        still_on = [name for name in wanted if after.get(name) is True]
        if still_on:
            raise AgentError(
                "tool_disable_failed",
                "以下工具未能关闭：" + ", ".join(still_on),
                status=502,
                step="apply_config",
            )


def uninstall_agent(
    host: HostClient,
    ledger: InstalledLedger,
    agent_id: str,
    *,
    sleep: Callable[[float], None] = time.sleep,
) -> dict[str, Any]:
    entry = ledger.get(agent_id)
    if entry is None:
        raise AgentError(
            "not_owned",
            "这个智能体不是本市场安装的，拒绝删除",
            status=404,
        )

    last: HostError | None = None
    for attempt in range(5):
        try:
            host.delete_agent(agent_id)
            last = None
            break
        except HostError as exc:
            if exc.status == 404:  # already gone: still clear the ledger
                last = None
                break
            last = exc
            if exc.status == 409 and attempt < 4:
                sleep(0.5)
                continue
            raise AgentError(
                exc.code, str(exc), status=exc.status or 502, step="delete_agent"
            ) from exc
    if last is not None:  # pragma: no cover - loop either breaks or raises
        raise AgentError(last.code, str(last), status=last.status or 502)

    ledger.remove(agent_id)
    return {
        "removed": True,
        "agent_id": agent_id,
        "workspace_kept": entry.get("workspace_dir"),
    }


def installed_items(
    host: HostClient,
    ledger: InstalledLedger,
    status_lookup: Callable[[str], dict[str, Any]],
) -> list[dict[str, Any]]:
    """Ledger entries reconciled against the host, with market state."""
    try:
        existing = {str(item.get("id") or "") for item in host.list_agents()}
    except HostError:
        # A transient host outage must not wipe the local ledger: we simply
        # cannot prove any entry is stale, so we keep them all.
        existing = None
    if existing is not None:
        ledger.reconcile(existing)

    items: list[dict[str, Any]] = []
    for entry in ledger.items():
        state = "unknown"
        detail: str | None = None
        try:
            state = str(status_lookup(str(entry.get("slug") or "")).get("state") or "unknown")
        except Exception as exc:  # noqa: BLE001 - status is best-effort
            status = getattr(exc, "status", 0)
            if status in (404, 410):
                state = "gone"
            else:
                detail = str(exc)
        item = dict(entry)
        item["state"] = state
        if detail:
            item["detail"] = detail
        items.append(item)
    return items
