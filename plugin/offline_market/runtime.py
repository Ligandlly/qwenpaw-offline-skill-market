# -*- coding: utf-8 -*-
"""Settings resolution: market URL, tenant identity, and the scan-mode gate.

Everything here is cheap and local — it is called from ``available()``, which
the host invokes on every provider listing and every search (see
spikes/findings-2.2.2.md 结论 8). No network I/O, no config parsing on the hot
path beyond a cached file read.
"""

from __future__ import annotations

import json
import os
import time
import urllib.parse
from dataclasses import dataclass, field
from pathlib import Path

VALID_SCAN_MODES = {"block", "warn", "off"}

# `block` stops an install; `warn` scans and logs but installs anyway — both mean
# the scanner actually looked at the skill. `off` scans nothing, which is the
# only mode this market refuses to distribute under.
ACCEPTED_SCAN_MODES = frozenset({"block", "warn"})

# Mode assumed when config.json is missing or says something unreadable.
ASSUMED_SCAN_MODE = "block"

# Not a real QwenPaw mode: we could not read the config at all.
UNKNOWN_MODE = "unknown"

# Where the market backend lives when neither the plugin config nor the env
# says otherwise: the single-machine default, which is also what
# deploy/docker-compose.yml publishes.
DEFAULT_MARKET_URL = "http://localhost:8080"

# ============================================================================
# 外置配置入口：插件根目录的 market.config.json（与 plugin.json 同级）。
#
# 典型场景：内网部署不想给每个租户 runtime 配 hub credentials / config.json。
# 只改 JSON、不改代码：{ "market_url": "http://10.137.157.214:8080" }。
# 可以直接编辑已安装插件副本里的这份文件（mtime/大小变化立即生效，
# 否则最迟 30 秒生效，无需重装插件）；打包前改源码里的这份则随插件包分发。
#
# 非空时优先级最高：config.json 的 plugins.offline-market.market_url 与
# OFFLINE_MARKET_URL 都会被忽略。留空 / 文件缺失 / JSON 解析失败 = 不覆盖，
# 顺序仍是：plugin_config.market_url > OFFLINE_MARKET_URL > DEFAULT_MARKET_URL
#
# 地址必须与后端的 MARKET_PUBLIC_URL 逐字符相同（docs/OPS.md §2.1），
# 否则安装会在宿主侧报 URL does not belong to Hub 'offline-market'。
# ============================================================================
MARKET_CONFIG_FILENAME = "market.config.json"

# market_url 在宿主每次列目录/搜索的热路径上被读（available()），所以常态
# 只花一次 stat()，文件变了才重新解析。但有的文件系统（如 overlayfs）对
# 快速连续写入给出相同的 mtime，同尺寸编辑会骗过 (mtime, size) 键，
# 所以另加 TTL 上限（与扫描模式门控的 _SCAN_MODE_TTL 同一口径）。
_MARKET_CONFIG_TTL = 30.0

# ((path, mtime_ns, size), fetched_at, url)
_market_config_cache: tuple[tuple[str, int, int], float, str] | None = None


def market_config_path() -> Path:
    """插件根目录（offline_market 包的上一层）里的 market.config.json。"""
    return Path(__file__).resolve().parent.parent / MARKET_CONFIG_FILENAME


def file_market_url() -> str:
    """market.config.json 里的后端地址；未配置/不可读时返回 ""。

    配置文件坏了不能把整个市场拖死：解析失败视同未配置，回退到常规优先级链。
    """
    global _market_config_cache
    path = market_config_path()
    try:
        stat = path.stat()
    except OSError:
        _market_config_cache = None
        return ""
    key = (str(path), stat.st_mtime_ns, stat.st_size)
    fetched_at = time.monotonic()
    if (
        _market_config_cache is not None
        and _market_config_cache[0] == key
        and fetched_at - _market_config_cache[1] < _MARKET_CONFIG_TTL
    ):
        return _market_config_cache[2]
    url = ""
    try:
        with open(path, encoding="utf-8") as handle:
            payload = json.load(handle)
        if isinstance(payload, dict) and isinstance(payload.get("market_url"), str):
            url = payload["market_url"].strip()
    except (OSError, ValueError):
        url = ""
    _market_config_cache = (key, fetched_at, url)
    return url

# Scan-mode config is re-read at most this often (seconds). Editing
# config.json or the env is an operator action, not a hot loop.
_SCAN_MODE_TTL = 30.0


def _first(*values: str | None) -> str | None:
    for value in values:
        if value and value.strip():
            return value.strip()
    return None


def _float_env(name: str, default: float) -> float:
    try:
        value = float(os.environ.get(name, ""))
    except ValueError:
        return default
    return value if value > 0 else default


def default_config_path() -> Path:
    """Where QwenPaw keeps config.json, without importing qwenpaw."""
    env = os.environ.get("QWENPAW_CONFIG_PATH")
    if env:
        return Path(env)
    home = os.environ.get("QWENPAW_WORKING_DIR")
    root = Path(home) if home else Path.home() / ".qwenpaw"
    return root / "config.json"


@dataclass(frozen=True)
class ScanGate:
    mode: str
    source: str
    detail: str

    @property
    def ok(self) -> bool:
        return self.mode in ACCEPTED_SCAN_MODES

    def reason(self) -> str:
        if self.mode == UNKNOWN_MODE:
            return (
                f"技能市场已暂停分发：读不到 QwenPaw 扫描配置（{self.detail}）。"
                "无法确认扫描处于开启状态，因此本市场拒绝服务。"
                "修复：把该 runtime 的 security.skill_scanner.mode 设为 block 或 warn 后重启。"
            )
        return (
            "技能市场已暂停分发：QwenPaw 技能扫描被关闭"
            f"（当前 {self.mode}，来源 {self.source}）。"
            "off 模式不扫描技能内容，任何东西都能被装进 runtime，"
            "因此本市场只在 block（拦截）或 warn（扫描后告警）下分发。"
            "修复：把该 runtime 的 security.skill_scanner.mode 设为 block 或 warn 后重启。"
        )


def resolve_scan_gate(config_path: Path | None = None) -> ScanGate:
    env = os.environ.get("QWENPAW_SKILL_SCAN_MODE")
    if env and env.lower().strip() in VALID_SCAN_MODES:
        return ScanGate(env.lower().strip(), "env", "QWENPAW_SKILL_SCAN_MODE")

    path = config_path or default_config_path()
    try:
        with open(path, encoding="utf-8") as handle:
            payload = json.load(handle)
        raw = (
            payload.get("security", {})
            .get("skill_scanner", {})
            .get("mode", ASSUMED_SCAN_MODE)
        )
        mode = str(raw).lower().strip()
        if mode in VALID_SCAN_MODES:
            return ScanGate(mode, "config", str(path))
        return ScanGate(ASSUMED_SCAN_MODE, "default", f"{path}: invalid mode {raw!r}")
    except FileNotFoundError:
        return ScanGate(ASSUMED_SCAN_MODE, "default", f"{path} missing")
    except Exception as exc:  # unreadable config: we cannot tell, so refuse
        return ScanGate(UNKNOWN_MODE, "unknown", f"cannot read {path}: {exc}")


@dataclass
class PluginSettings:
    plugin_config: dict = field(default_factory=dict)
    config_path: Path | None = None

    # ---- resolved lazily; env can change only on restart anyway ----
    @property
    def market_url(self) -> str:
        raw = _first(
            file_market_url(),
            str(self.plugin_config.get("market_url") or ""),
            os.environ.get("OFFLINE_MARKET_URL"),
        ) or DEFAULT_MARKET_URL
        return raw.rstrip("/")

    def market_url_problem(self) -> str | None:
        """Reject addresses urllib cannot use, with the typo in the message.

        A literal space (``http://10 .10.145.48:8080``) used to escape as an
        unhandled ``http.client.InvalidURL`` and surface as HTTP 500.
        """
        url = self.market_url
        if any(char.isspace() for char in url):
            return (
                f"市场地址包含空白字符：{url!r}；"
                "请改成连续地址（例如 http://10.10.145.48:8080）"
            )
        try:
            parsed = urllib.parse.urlsplit(url)
            _ = parsed.port  # validates a malformed/non-numeric port
        except ValueError as exc:
            return f"市场地址无效：{url!r}（{exc}）"
        if parsed.scheme not in {"http", "https"} or not parsed.hostname:
            return f"市场地址必须是 http(s)://主机[:端口]：{url!r}"
        return None

    @property
    def tenant(self) -> str | None:
        """The only identity source the host gives us in the market path."""
        return _first(
            os.environ.get("QWENPAW_TENANT_ID"),
            os.environ.get("TENANT_ID"),
            os.environ.get("OFFLINE_MARKET_TENANT"),
        )

    @property
    def standalone_tenant(self) -> bool:
        return not _first(
            os.environ.get("QWENPAW_TENANT_ID"),
            os.environ.get("TENANT_ID"),
        ) and bool(_first(os.environ.get("OFFLINE_MARKET_TENANT")))

    @property
    def timeout(self) -> float:
        try:
            return float(os.environ.get("OFFLINE_MARKET_TIMEOUT", "10"))
        except ValueError:
            return 10.0

    # ---- host API (this runtime's own QwenPaw app) ----
    @property
    def host_api_url(self) -> str | None:
        """Explicit override; ``resolve_base_url`` handles the rest."""
        return _first(
            str(self.plugin_config.get("host_api_url") or ""),
            os.environ.get("OFFLINE_MARKET_HOST_API_URL"),
        )

    @property
    def host_timeout(self) -> float:
        return _float_env("OFFLINE_MARKET_HOST_TIMEOUT", 30.0)

    @property
    def install_ready_timeout(self) -> float:
        return _float_env("OFFLINE_MARKET_INSTALL_TIMEOUT", 60.0)

    @property
    def install_poll(self) -> float:
        return _float_env("OFFLINE_MARKET_INSTALL_POLL", 0.5)

    # ---- local state (install ledger) ----
    @property
    def working_dir(self) -> Path:
        raw = _first(os.environ.get("QWENPAW_WORKING_DIR"))
        return Path(raw).expanduser() if raw else Path.home() / ".qwenpaw"

    @property
    def state_dir(self) -> Path:
        return self.working_dir / "offline-market"

    def scan_gate(self) -> ScanGate:
        return resolve_scan_gate(self.config_path)

    def problems(self) -> list[str]:
        """Reasons this market cannot serve, most fundamental first."""
        out: list[str] = []
        url_problem = self.market_url_problem()
        if url_problem:
            out.append(url_problem)
        gate = self.scan_gate()
        if not gate.ok:
            out.append(gate.reason())
        if not self.tenant:
            out.append(
                "无法确定租户身份：QWENPAW_TENANT_ID 为空。"
                "本市场只对 QwenPaw Hub 托管的租户 runtime 提供服务；"
                "单机部署请显式设置 OFFLINE_MARKET_TENANT（身份将退化为配置字符串）"
            )
        return out
