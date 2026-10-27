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
# 硬编码入口：只改下面这一行，就能把市场地址写死进插件。
#
# 典型场景：内网打一次性插件包，不想给每个租户 runtime 配 hub credentials /
# config.json。把 "" 改成后端地址（如 "http://10.137.157.214:8080"），
# 重装插件（qwenpaw plugin install ./plugin --force）即可生效。
#
# 非空时优先级最高：config.json 的 plugins.offline-market.market_url 与
# OFFLINE_MARKET_URL 都会被忽略。留空（默认）= 不覆盖，顺序仍是：
#   plugin_config.market_url > OFFLINE_MARKET_URL > DEFAULT_MARKET_URL
#
# 地址必须与后端的 MARKET_PUBLIC_URL 逐字符相同（docs/OPS.md §2.1），
# 否则安装会在宿主侧报 URL does not belong to Hub 'offline-market'。
# ============================================================================
HARDCODED_MARKET_URL = ""

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
            HARDCODED_MARKET_URL,
            str(self.plugin_config.get("market_url") or ""),
            os.environ.get("OFFLINE_MARKET_URL"),
        ) or DEFAULT_MARKET_URL
        return raw.rstrip("/")

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
