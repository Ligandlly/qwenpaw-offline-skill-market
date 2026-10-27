# -*- coding: utf-8 -*-
"""Snapshot, install state machine, rollback, ledger and uninstall."""

from __future__ import annotations

import base64
import io
import json
import zipfile
from pathlib import Path

import pytest

from offline_market.agents import (
    AgentError,
    AgentInstaller,
    InstalledLedger,
    build_skills_zip,
    installed_items,
    parse_bundle,
    snapshot_agent,
    uninstall_agent,
)
from offline_market.host import HostError

GOOD_SKILL = "---\nname: demo\ndescription: a demo\n---\n\n# demo\n"


class FakeHost:
    def __init__(
        self,
        *,
        agents: list[dict] | None = None,
        config: dict | None = None,
        skills: list[dict] | None = None,
        tools: list[dict] | None = None,
        ready_failures: int = 0,
        upload_error: Exception | None = None,
        delete_error: Exception | None = None,
    ) -> None:
        self.agents = agents or []
        self.config = config or {}
        self.skills = skills or []
        self.tools = tools or []
        self.ready_failures = ready_failures
        self.upload_error = upload_error
        self.delete_error = delete_error
        self.calls: list[tuple[str, object]] = []
        self.created: dict | None = None
        self.uploaded: bytes | None = None

    def _record(self, name: str, payload: object = None) -> None:
        self.calls.append((name, payload))

    def list_agents(self):
        self._record("list_agents")
        return self.agents

    def get_agent(self, agent_id):
        self._record("get_agent", agent_id)
        return self.config

    def create_agent(self, payload):
        self._record("create_agent", payload)
        self.created = payload
        return {
            "id": payload["id"],
            "workspace_dir": f"/tmp/ws-{payload['id']}",
            "enabled": True,
        }

    def delete_agent(self, agent_id):
        self._record("delete_agent", agent_id)
        if self.delete_error:
            raise self.delete_error
        return {"success": True}

    def update_agent(self, agent_id, patch):
        self._record("update_agent", (agent_id, patch))
        return {}

    def list_workspace_files(self, agent_id):
        self._record("list_workspace_files", agent_id)
        if self.ready_failures > 0:
            self.ready_failures -= 1
            raise HostError(404, "not_ready", "starting")
        return []

    def write_workspace_file(self, agent_id, name, content):
        self._record("write_workspace_file", (agent_id, name, content))
        return {"written": True}

    def list_skills(self, agent_id):
        self._record("list_skills", agent_id)
        return self.skills

    def upload_skills_zip(self, agent_id, data):
        self._record("upload_skills_zip", (agent_id, data))
        self.uploaded = data
        if self.upload_error:
            raise self.upload_error
        return {"imported": ["demo"], "count": 1}

    def list_tools(self, agent_id):
        self._record("list_tools", agent_id)
        return [dict(item) for item in self.tools]

    def toggle_tool(self, agent_id, tool_name):
        self._record("toggle_tool", (agent_id, tool_name))
        for item in self.tools:
            if item.get("name") == tool_name:
                item["enabled"] = not item.get("enabled")
        return {}


def make_bundle(profile: dict | None = None, version: str = "1.0.0") -> dict:
    manifest = {
        "schema": 1,
        "name": "QA Agent",
        "description": "demo agent",
        "language": "zh",
        "profile": profile
        if profile is not None
        else {
            "thinking_level": "medium",
            "plan_enabled": True,
            "disabled_tools": ["browser"],
        },
        "md_files": ["SOUL.md"],
        "skills": ["demo"],
    }
    return {
        "name": "QA Agent",
        "version": version,
        "files": {
            "manifest.json": json.dumps(manifest),
            "SOUL.md": "# soul\n",
            "skills/demo/SKILL.md": GOOD_SKILL,
            "skills/demo/scripts/run.py": "print(1)\n",
        },
    }


def ledger(tmp_path: Path) -> InstalledLedger:
    return InstalledLedger(tmp_path / "state" / "installed-agents.json")


def installer(host: FakeHost, store: InstalledLedger) -> AgentInstaller:
    return AgentInstaller(
        host, store, ready_timeout=1.0, poll=0.0, sleep=lambda _seconds: None
    )


def test_snapshot_collects_only_persona_and_enabled_skills(tmp_path):
    workspace = tmp_path / "workspaces" / "qa"
    (workspace / "skills" / "demo" / "scripts").mkdir(parents=True)
    (workspace / "skills" / "demo" / "references").mkdir(parents=True)
    (workspace / "skills" / "demo" / "__pycache__").mkdir(parents=True)
    (workspace / "SOUL.md").write_text("# soul\n", encoding="utf-8")
    (workspace / "AGENTS.md").write_text("# agents\n", encoding="utf-8")
    (workspace / "MEMORY.md").write_text("private\n", encoding="utf-8")
    (workspace / "skills" / "demo" / "SKILL.md").write_text(GOOD_SKILL, encoding="utf-8")
    (workspace / "skills" / "demo" / "scripts" / "run.py").write_text(
        "print(1)\n", encoding="utf-8"
    )
    blob = b"\xff\x00binary"
    (workspace / "skills" / "demo" / "references" / "blob.bin").write_bytes(blob)
    (workspace / "skills" / "demo" / "__pycache__" / "x.pyc").write_bytes(b"junk")

    host = FakeHost(
        config={
            "id": "qa",
            "name": "QA Agent",
            "description": "demo agent",
            "language": "zh",
            "workspace_dir": str(workspace),
            "backend": "qwenpaw",
            "thinking_level": "medium",
            "plan": {"enabled": True},
            "tools": {
                "builtin_tools": {
                    "browser": {"enabled": False},
                    "shell": {"enabled": True},
                }
            },
        },
        skills=[
            {"name": "demo", "enabled": True},
            {"name": "off", "enabled": False},
        ],
    )
    snapshot = snapshot_agent(host, "qa")
    files = snapshot["files"]

    assert "MEMORY.md" not in files
    assert "skills/demo/__pycache__/x.pyc" not in files
    assert files["SOUL.md"] == "# soul\n"
    assert files["skills/demo/scripts/run.py"] == "print(1)\n"
    assert files["skills/demo/references/blob.bin"] == {
        "base64": base64.b64encode(blob).decode("ascii")
    }

    manifest = snapshot["manifest"]
    assert manifest["md_files"] == ["AGENTS.md", "SOUL.md"]
    assert manifest["skills"] == ["demo"]
    assert manifest["profile"]["plan_enabled"] is True
    assert manifest["profile"]["disabled_tools"] == ["browser"]
    assert "off" not in files and "skills/off/SKILL.md" not in files


def test_snapshot_keeps_host_skill_names_verbatim(tmp_path):
    workspace = tmp_path / "workspaces" / "qa"
    names = ["QA_source_index", "周报助手", "with space"]
    for name in names:
        skill_dir = workspace / "skills" / name
        skill_dir.mkdir(parents=True)
        (skill_dir / "SKILL.md").write_text(
            f"---\nname: {name}\ndescription: demo\n---\n\n# {name}\n",
            encoding="utf-8",
        )
    host = FakeHost(
        config={
            "name": "QA",
            "description": "existing host skill names",
            "workspace_dir": str(workspace),
            "backend": "qwenpaw",
        },
        skills=[{"name": name, "enabled": True} for name in names],
    )

    snapshot = snapshot_agent(host, "qa")
    assert snapshot["manifest"]["skills"] == names
    for name in names:
        assert f"skills/{name}/SKILL.md" in snapshot["files"]

    data = build_skills_zip(snapshot["files"], snapshot["manifest"]["skills"])
    assert data is not None
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for name in names:
            assert f"{name}/SKILL.md" in archive.namelist()


def test_parse_bundle_accepts_host_skill_names():
    names = ["QA_source_index", "周报助手", "with space"]
    manifest = {
        "schema": 1,
        "name": "QA",
        "language": "zh",
        "md_files": ["SOUL.md"],
        "skills": names,
    }
    files = {
        "manifest.json": json.dumps(manifest, ensure_ascii=False),
        "SOUL.md": "# soul\n",
    }
    for name in names:
        files[f"skills/{name}/SKILL.md"] = (
            f"---\nname: {name}\ndescription: demo\n---\n"
        )
    assert parse_bundle(files)["skills"] == names


def test_parse_bundle_still_rejects_path_like_skill_names():
    for name in ("a/b", "a\\b", ".", "..", " leading", "trailing ", "x" * 129):
        manifest = {
            "schema": 1,
            "name": "QA",
            "md_files": ["SOUL.md"],
            "skills": [name],
        }
        files = {
            "manifest.json": json.dumps(manifest, ensure_ascii=False),
            "SOUL.md": "# soul\n",
            f"skills/{name}/SKILL.md": "---\nname: demo\ndescription: demo\n---\n",
        }
        with pytest.raises(AgentError) as excinfo:
            parse_bundle(files)
        assert excinfo.value.code == "bad_bundle", name


def test_snapshot_reports_non_text_persona(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "SOUL.md").write_bytes(b"\xff\xfe")
    host = FakeHost(
        config={
            "name": "QA",
            "workspace_dir": str(workspace),
            "backend": "qwenpaw",
        }
    )
    with pytest.raises(AgentError) as excinfo:
        snapshot_agent(host, "qa")
    assert excinfo.value.code == "invalid_markdown"


def test_zip_is_rebuilt_with_skill_directories():
    files = {
        "skills/demo/SKILL.md": GOOD_SKILL,
        "skills/demo/scripts/run.py": "print(1)\n",
        "skills/demo/refs/a.bin": {"base64": base64.b64encode(b"\x00\xff").decode()},
    }
    data = build_skills_zip(files, ["demo"])
    assert data is not None
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        assert sorted(archive.namelist()) == [
            "demo/SKILL.md",
            "demo/refs/a.bin",
            "demo/scripts/run.py",
        ]
        assert archive.read("demo/refs/a.bin") == b"\x00\xff"


def test_parse_bundle_rejects_path_traversal():
    manifest = {
        "schema": 1,
        "name": "QA",
        "md_files": ["SOUL.md"],
        "skills": [],
    }
    files = {
        "manifest.json": json.dumps(manifest),
        "SOUL.md": "# soul",
        "skills/../escape.py": "print(1)",
    }
    with pytest.raises(AgentError) as excinfo:
        parse_bundle(files)
    assert excinfo.value.code == "bad_bundle"


def test_install_runs_the_expected_sequence_and_records_ledger(tmp_path):
    host = FakeHost(tools=[{"name": "browser", "enabled": True}])
    store = ledger(tmp_path)
    result = installer(host, store).install(
        slug="qa-agent", bundle=make_bundle(), target_name="内网 QA"
    )

    assert result["agent_id"] == "qa-agent"
    assert result["name"] == "内网 QA"
    assert result["workspace_dir"] == "/tmp/ws-qa-agent"
    assert result["warnings"] == []

    names = [name for name, _payload in host.calls]
    assert names == [
        "list_agents",
        "create_agent",
        "list_workspace_files",
        "write_workspace_file",
        "upload_skills_zip",
        "update_agent",
        "list_tools",
        "toggle_tool",
        "list_tools",
    ]
    payload = host.created
    assert payload["language"] == "zh"
    assert payload["thinking_level"] == "medium"
    assert payload["skill_names"] == []

    with zipfile.ZipFile(io.BytesIO(host.uploaded)) as archive:
        assert sorted(archive.namelist()) == [
            "demo/SKILL.md",
            "demo/scripts/run.py",
        ]

    entry = store.get("qa-agent")
    assert entry and entry["slug"] == "qa-agent" and entry["version"] == "1.0.0"


def test_install_retries_until_the_workspace_is_ready(tmp_path):
    host = FakeHost(ready_failures=2)
    result = installer(host, ledger(tmp_path)).install(
        slug="qa-agent", bundle=make_bundle()
    )
    assert result["agent_id"] == "qa-agent"
    waits = [name for name, _ in host.calls if name == "list_workspace_files"]
    assert len(waits) == 3


def test_install_uses_a_suffix_when_the_host_id_is_taken(tmp_path):
    host = FakeHost(agents=[{"id": "qa-agent"}])
    result = installer(host, ledger(tmp_path)).install(
        slug="qa-agent", bundle=make_bundle()
    )
    assert result["agent_id"] == "qa-agent-2"


def test_already_installed_slug_is_rejected_without_touching_the_host(tmp_path):
    store = ledger(tmp_path)
    store.add(
        {
            "agent_id": "qa-agent",
            "slug": "qa-agent",
            "version": "0.9.0",
            "name": "old",
            "workspace_dir": "/tmp/old",
            "installed_at": 1,
        }
    )
    host = FakeHost()
    with pytest.raises(AgentError) as excinfo:
        installer(host, store).install(slug="qa-agent", bundle=make_bundle())
    assert excinfo.value.status == 409
    assert host.calls == []


def test_failed_skill_upload_rolls_back_the_created_agent(tmp_path):
    error = HostError(422, "security_scan_failed", "CRITICAL: exec(input())")
    host = FakeHost(upload_error=error)
    store = ledger(tmp_path)
    with pytest.raises(AgentError) as excinfo:
        installer(host, store).install(slug="qa-agent", bundle=make_bundle())

    raised = excinfo.value
    assert raised.code == "security_scan_failed"
    assert raised.step == "install_skills"
    assert raised.rollback_failed is False
    assert raised.workspace_kept == "/tmp/ws-qa-agent"
    assert ("delete_agent", "qa-agent") in host.calls
    assert store.get("qa-agent") is None


def test_uninstall_only_deletes_agents_in_the_ledger(tmp_path):
    store = ledger(tmp_path)
    store.add(
        {
            "agent_id": "qa-agent",
            "slug": "qa-agent",
            "version": "1.0.0",
            "name": "QA",
            "workspace_dir": "/tmp/ws-qa-agent",
            "installed_at": 1,
        }
    )
    host = FakeHost()
    result = uninstall_agent(host, store, "qa-agent")
    assert result == {
        "removed": True,
        "agent_id": "qa-agent",
        "workspace_kept": "/tmp/ws-qa-agent",
    }
    assert store.get("qa-agent") is None
    assert ("delete_agent", "qa-agent") in host.calls

    with pytest.raises(AgentError) as excinfo:
        uninstall_agent(host, store, "not-ours")
    assert excinfo.value.status == 404


def test_installed_items_reconciles_and_carries_market_state(tmp_path):
    store = ledger(tmp_path)
    store.add(
        {
            "agent_id": "qa-agent",
            "slug": "qa-agent",
            "version": "1.0.0",
            "name": "QA",
            "workspace_dir": "/tmp/ws-qa-agent",
            "installed_at": 1,
        }
    )
    host = FakeHost(agents=[{"id": "qa-agent"}])
    items = installed_items(host, store, lambda slug: {"state": "taken_down"})
    assert items[0]["state"] == "taken_down"

    host.agents = []
    assert installed_items(host, store, lambda slug: {"state": "active"}) == []
    assert store.get("qa-agent") is None
