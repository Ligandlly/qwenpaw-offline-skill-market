#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""部署前/部署后自检：市场后端 + 扫描闸门 + 租户 runtime 配置。

只读，不改任何东西；任何一项 FAIL 都会让退出码非 0，方便塞进发布流程。

    python3 preflight.py --market-url http://market.internal:8080 \
        --admin-password "$ADMIN_PASSWORD" \
        --app-url http://127.0.0.1:8123 \
        --runtimes-dir /var/lib/qwenpaw-hub/runtimes
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

GREEN, RED, YELLOW, RESET = "\033[32m", "\033[31m", "\033[33m", "\033[0m"
failures = 0


def report(ok: bool, label: str, detail: str = "", warn: bool = False) -> None:
    global failures
    tag = f"{GREEN}PASS{RESET}" if ok else (f"{YELLOW}WARN{RESET}" if warn else f"{RED}FAIL{RESET}")
    print(f"  [{tag}] {label}" + (f" — {detail}" if detail else ""))
    if not ok and not warn:
        failures += 1


def http(method: str, url: str, body=None, headers=None, timeout=10):
    data = json.dumps(body).encode() if body is not None else None
    request_headers = {"accept": "application/json"}
    if data:
        request_headers["content-type"] = "application/json"
    request_headers.update(headers or {})
    request = urllib.request.Request(url, data=data, headers=request_headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw else {}), response.headers
    except urllib.error.HTTPError as exc:
        raw = exc.read()
        try:
            payload = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            payload = {}
        return exc.code, payload, exc.headers
    except urllib.error.URLError as exc:
        return 0, {"error": str(exc.reason)}, {}


def check_market(market_url: str, admin: str, password: str) -> None:
    print("\n市场后端")
    base = market_url.rstrip("/")
    status, _, _ = http("GET", f"{base}/health")
    report(status == 200, "GET /health", str(status))
    if status != 200:
        return

    status, limits, _ = http("GET", f"{base}/api/v1/limits", headers={"x-tenant-id": "preflight"})
    report(status == 200, "GET /api/v1/limits（限额已配置）", str(status))
    if status == 200:
        print(
            f"        单包 {limits.get('max_bundle_bytes')} B / 文件 {limits.get('max_files')} 个"
            f" / 单文件 {limits.get('max_file_bytes')} B / 深度 {limits.get('max_path_depth')}"
        )

    if password:
        status, _, headers = http(
            "POST", f"{base}/admin/api/login", {"username": admin, "password": password}
        )
        cookie = headers.get("set-cookie", "").split(";", 1)[0]
        report(status == 200 and bool(cookie), "管理员登录", str(status))
        if status == 200:
            status, audit, _ = http("GET", f"{base}/admin/api/audit?limit=1", headers={"cookie": cookie})
            report(status == 200, "审计可读", str(status))
    else:
        report(False, "管理员登录", "未提供 --admin-password，跳过", warn=True)


def check_scan_dirs(paths: list[Path]) -> None:
    print("\n租户 runtime 扫描模式（必须 block）")
    if not paths:
        report(False, "未提供 --runtimes-dir", "无法校验租户 runtime 配置", warn=True)
        return
    offenders = []
    good = 0
    for path in paths:
        try:
            payload = json.loads(path.read_text(encoding="utf-8") or "{}")
            mode = payload.get("security", {}).get("skill_scanner", {}).get("mode")
        except Exception as exc:  # noqa: BLE001
            offenders.append(f"{path}（读取失败：{exc}）")
            continue
        if mode == "block":
            good += 1
        else:
            offenders.append(f"{path}（mode={mode!r}）")
    report(not offenders, f"{good}/{len(paths)} 个 config.json 为 block", "；".join(offenders[:5]))
    if offenders:
        print("        修复：deploy/scripts/enforce-scan-mode.py --runtimes-dir <hub runtimes>")


def check_app(app_url: str) -> None:
    print("\nQwenPaw 控制台")
    base = app_url.rstrip("/")
    status, providers, _ = http("GET", f"{base}/api/market/providers")
    if status != 200:
        report(False, "GET /api/market/providers", str(status))
        return
    ours = [item for item in providers if item.get("key") == "offline-market"]
    report(bool(ours), "插件已加载（offline-market provider）")
    if ours:
        report(ours[0].get("available") is True, "provider 可用", str(ours[0].get("reason") or ""))


def check_plugin_installed(qwenpaw: str) -> None:
    print("\n插件安装状态")
    try:
        # the qwenpaw CLI prints UTF-8 (box drawing/emoji); Windows defaults to
        # GBK and would raise UnicodeDecodeError inside subprocess
        completed = subprocess.run(
            [qwenpaw, "plugin", "list"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=120,
        )
        output = (completed.stdout or "") + (completed.stderr or "")
    except Exception as exc:  # noqa: BLE001
        report(False, f"{qwenpaw} plugin list", str(exc), warn=True)
        return
    report("offline-market" in output, "qwenpaw plugin list 含 offline-market")


def main() -> int:
    parser = argparse.ArgumentParser(description="offline skill market preflight")
    parser.add_argument("--market-url", required=True)
    parser.add_argument("--admin", default="admin")
    parser.add_argument("--admin-password", default=os.environ.get("ADMIN_PASSWORD", ""))
    parser.add_argument("--app-url", help="QwenPaw 控制台地址（可选）")
    parser.add_argument("--runtimes-dir", help="hub runtimes 目录（可选）")
    parser.add_argument("--qwenpaw", help="qwenpaw 可执行文件路径（可选）")
    args = parser.parse_args()

    print("离线技能市场 preflight")
    check_market(args.market_url, args.admin, args.admin_password)
    if args.runtimes_dir:
        root = Path(args.runtimes_dir)
        configs = sorted(root.rglob("config.json")) if root.is_dir() else []
        check_scan_dirs(configs)
    else:
        check_scan_dirs([])
    if args.app_url:
        check_app(args.app_url)
    if args.qwenpaw:
        check_plugin_installed(args.qwenpaw)

    print()
    if failures:
        print(f"{RED}{failures} 项未通过{RESET}")
        return 1
    print(f"{GREEN}全部通过{RESET}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
