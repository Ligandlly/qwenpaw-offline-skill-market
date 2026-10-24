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
REQUIRED_SCAN_MODE = "block"

# Scan-mode config is re-read at most this often (seconds). Editing
# config.json or the env is an operator action, not a hot loop.
_SCAN_MODE_TTL = 30.0


def _first(*values: str | None) -> str | None:
    for value in values:
        if value and value.strip():
            return value.strip()
    return None


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
        return self.mode == REQUIRED_SCAN_MODE

    def reason(self) -> str:
        return (
            "技能市场已暂停分发：QwenPaw 技能扫描未处于 block 模式"
            f"（当前 {self.mode}，来源 {self.source}）。"
            "默认的 warn 模式只会记录告警并照常安装，因此本市场拒绝服务。"
            "修复：为该 runtime 设置 QWENPAW_SKILL_SCAN_MODE=block 后重启。"
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
            .get("mode", REQUIRED_SCAN_MODE)
        )
        mode = str(raw).lower().strip()
        if mode in VALID_SCAN_MODES:
            return ScanGate(mode, "config", str(path))
        return ScanGate(REQUIRED_SCAN_MODE, "default", f"{path}: invalid mode {raw!r}")
    except FileNotFoundError:
        return ScanGate(REQUIRED_SCAN_MODE, "default", f"{path} missing")
    except Exception as exc:  # pragma: no cover - unreadable config is rare
        return ScanGate("warn", "unknown", f"cannot read {path}: {exc}")


@dataclass
class PluginSettings:
    plugin_config: dict = field(default_factory=dict)
    config_path: Path | None = None

    # ---- resolved lazily; env can change only on restart anyway ----
    @property
    def market_url(self) -> str | None:
        raw = _first(
            str(self.plugin_config.get("market_url") or ""),
            os.environ.get("OFFLINE_MARKET_URL"),
        )
        return raw.rstrip("/") if raw else None

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

    def scan_gate(self) -> ScanGate:
        return resolve_scan_gate(self.config_path)

    def problems(self) -> list[str]:
        """Reasons this market cannot serve, most fundamental first."""
        out: list[str] = []
        if not self.market_url:
            out.append(
                "未配置市场后端地址：请在插件配置里填写 market_url，"
                "或为该 runtime 设置 OFFLINE_MARKET_URL"
            )
            return out
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
