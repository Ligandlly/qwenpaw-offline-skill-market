# -*- coding: utf-8 -*-
"""Settings + the scan-mode gate (the thing S5 proved matters)."""

from __future__ import annotations

import json

from offline_market.runtime import PluginSettings, resolve_scan_gate


def test_market_url_from_env_is_trimmed(env):
    settings = PluginSettings()
    assert settings.market_url == "http://market.test"


def test_market_url_from_plugin_config_wins(env):
    settings = PluginSettings({"market_url": "http://configured.test/"})
    assert settings.market_url == "http://configured.test"


def test_tenant_comes_from_runtime_env(env):
    assert PluginSettings().tenant == "tenant-a"
    assert PluginSettings().standalone_tenant is False


def test_standalone_tenant_is_flagged(env):
    env.delenv("QWENPAW_TENANT_ID")
    env.setenv("OFFLINE_MARKET_TENANT", "solo")
    settings = PluginSettings()
    assert settings.tenant == "solo"
    assert settings.standalone_tenant is True


def test_scan_gate_reads_env(env):
    gate = resolve_scan_gate()
    assert gate.mode == "block" and gate.ok


def test_scan_gate_accepts_warn_from_config(env, tmp_path):
    """warn still scans, it just installs anyway - tolerated on purpose."""
    env.delenv("QWENPAW_SKILL_SCAN_MODE")
    cfg = tmp_path / "config.json"
    cfg.write_text(json.dumps({"security": {"skill_scanner": {"mode": "warn"}}}), encoding="utf-8")
    gate = resolve_scan_gate(cfg)
    assert gate.mode == "warn" and gate.ok


def test_scan_gate_rejects_off_from_config(env, tmp_path):
    """off scans nothing, so the market refuses to distribute."""
    env.delenv("QWENPAW_SKILL_SCAN_MODE")
    cfg = tmp_path / "config.json"
    cfg.write_text(json.dumps({"security": {"skill_scanner": {"mode": "off"}}}), encoding="utf-8")
    gate = resolve_scan_gate(cfg)
    assert gate.mode == "off" and not gate.ok
    assert "off" in gate.reason()


def test_unreadable_config_has_no_gate(env, tmp_path):
    env.delenv("QWENPAW_SKILL_SCAN_MODE")
    gate = resolve_scan_gate(tmp_path)  # a directory: json.load cannot read it
    assert not gate.ok and "读不到" in gate.reason()


def test_scan_gate_prefers_env_over_config(env, tmp_path):
    cfg = tmp_path / "config.json"
    cfg.write_text(json.dumps({"security": {"skill_scanner": {"mode": "off"}}}), encoding="utf-8")
    assert resolve_scan_gate(cfg).mode == "block"  # env says block


def test_missing_config_assumes_block(env, tmp_path):
    env.delenv("QWENPAW_SKILL_SCAN_MODE")
    gate = resolve_scan_gate(tmp_path / "absent.json")
    assert gate.mode == "block"


def test_problems_reports_every_blocker_in_order(env, tmp_path):
    cfg = tmp_path / "config.json"
    cfg.write_text(json.dumps({"security": {"skill_scanner": {"mode": "off"}}}), encoding="utf-8")
    env.delenv("QWENPAW_SKILL_SCAN_MODE")
    env.delenv("QWENPAW_TENANT_ID")
    problems = PluginSettings(config_path=cfg).problems()
    assert len(problems) == 2
    assert "off" in problems[0]
    assert "QWENPAW_TENANT_ID" in problems[1]


def test_market_url_defaults_to_localhost(env):
    env.delenv("OFFLINE_MARKET_URL")
    assert PluginSettings().market_url == "http://localhost:8080"


def test_default_market_url_is_trimmed_of_trailing_slash(env):
    env.setenv("OFFLINE_MARKET_URL", "http://market.test//")
    assert PluginSettings().market_url == "http://market.test"
