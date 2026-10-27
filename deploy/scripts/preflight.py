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

try:
    from runtime_probe import parse_result, run_python
except ImportError:  # run from a different cwd
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from runtime_probe import parse_result, run_python

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
            f" / 单文件 {limits.get('max_file_bytes')} B"
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


def advertised_public_url(market_url: str) -> str | None:
    """后端按 MARKET_PUBLIC_URL 现算的 install_url 前缀（它才是权威值）。"""
    status, payload, _ = http(
        "GET",
        f"{market_url.rstrip('/')}/api/v1/skills?limit=1",
        headers={"x-tenant-id": "preflight"},
    )
    if status != 200:
        return None
    items = payload.get("items") if isinstance(payload, dict) else None
    for item in items or []:
        url = item.get("install_url")
        if url:
            return url.rsplit("/skills/", 1)[0]
    return None


def check_public_url(market_url: str, public_url: str) -> None:
    print("\n后端对外广告的地址（MARKET_PUBLIC_URL）")
    advertised = advertised_public_url(market_url)
    if advertised is None:
        report(False, "读取 install_url", "后端不可达或还没有技能可比对", warn=True)
        return
    report(
        advertised.rstrip("/") == public_url.rstrip("/"),
        "install_url 前缀 == MARKET_PUBLIC_URL",
        f"后端广告 {advertised!r}，配置 {public_url!r}",
    )


ACCEPTED_SCAN_MODES = {"block", "warn"}


def check_scan_dirs(paths: list[Path]) -> None:
    print("\n租户 runtime 扫描模式（block 推荐，warn 允许，off 拒绝）")
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
        if mode in ACCEPTED_SCAN_MODES:
            good += 1
        else:
            offenders.append(f"{path}（mode={mode!r}）")
    report(
        not offenders,
        f"{good}/{len(paths)} 个 config.json 扫描已开启（block/warn）",
        "；".join(offenders[:5]),
    )
    if offenders:
        print(
            "        修复：把 security.skill_scanner.mode 改成 block（推荐）或 warn；"
            "或跑 deploy/scripts/enforce-scan-mode.py --runtimes-dir <hub runtimes>"
        )


def check_runtime_urls(public_url: str, paths: list[Path]) -> str | None:
    """插件配置里的 market_url 必须与后端 MARKET_PUBLIC_URL 逐字符相同。

    不一致的症状不是"连不上"，而是安装时宿主侧报
    ``URL does not belong to Hub 'offline-market'``（见 OPS §2.1）。
    返回 runtime 实际配置的地址，供容器内探活用。
    """
    print("\n租户 runtime 的市场地址（必须与 MARKET_PUBLIC_URL 逐字符相同）")
    base = public_url.rstrip("/")
    if not paths:
        report(False, "未提供 --runtimes-dir", "无法校验租户 runtime 地址", warn=True)
        return None
    offenders = []
    checked = 0
    runtime_url = None
    for path in paths:
        try:
            payload = json.loads(path.read_text(encoding="utf-8") or "{}")
        except Exception as exc:  # noqa: BLE001
            offenders.append(f"{path}（读取失败：{exc}）")
            continue
        raw = (payload.get("plugins", {}).get("offline-market") or {}).get("market_url")
        if not raw:
            continue  # 没配就走 credentials/默认值，这里不下结论
        checked += 1
        runtime_url = runtime_url or str(raw)
        if str(raw).rstrip("/") != base:
            offenders.append(f"{path}（market_url={raw!r}）")
    if not offenders:
        report(True, f"{checked} 个 config.json 的 market_url 与后端一致")
    else:
        report(False, "market_url 与 MARKET_PUBLIC_URL 不一致", "；".join(offenders[:5]))
    return runtime_url


def check_runtime_container(container: str, market_url: str) -> None:
    """从 runtime 容器内部探一次后端。

    宿主机能访问不代表容器能访问：Docker Desktop（Windows）把发布端口只挂在
    127.0.0.1 与网桥地址上，宿主机自己的局域网 IP 往往不通（OPS §2.5）。
    """
    print("\n租户 runtime 容器内的后端可达性")
    url = f"{market_url.rstrip('/')}/health"
    if not container:
        report(False, "未提供 --runtime-container", "容器视角的地址未校验", warn=True)
        return
    probes = [
        ["python3", "-c", f"import urllib.request;print(urllib.request.urlopen({url!r},timeout=8).status)"],
        ["python", "-c", f"import urllib.request;print(urllib.request.urlopen({url!r},timeout=8).status)"],
        ["curl", "-sS", "-m", "8", "-o", "/dev/null", "-w", "%{http_code}", url],
    ]
    output = ""
    for probe in probes:
        try:
            completed = subprocess.run(
                ["docker", "exec", container, *probe],
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=60,
            )
        except Exception as exc:  # noqa: BLE001
            report(False, f"docker exec {container}", str(exc), warn=True)
            return
        output = ((completed.stdout or "") + (completed.stderr or "")).strip()
        if completed.returncode == 0 and "200" in output:
            break
    if "200" in output:
        report(True, f"容器内 GET {url}")
    else:
        report(False, f"容器内 GET {url}", output[:200] or "无输出")
        print("        修复：换成容器视角可达的地址（host.docker.internal / 同网络容器名），"
              "并同步改后端 MARKET_PUBLIC_URL（见 OPS §2.5）")


def check_runtime_agent_api(container: str) -> None:
    """From inside the runtime: plugin route -> its own host API.

    This is the Docker-specific failure mode the host-side checks cannot see:
    the published port is bound to a random loopback port, auth is on, and the
    runtime boundary requires ``X-QwenPaw-Runtime-Token``.
    """
    print("\n租户 runtime 容器内的插件 / 宿主 API")
    if not container:
        report(False, "未提供 --runtime-container", "宿主 API 自检未执行", warn=True)
        return
    try:
        completed = run_python(container, RUNTIME_PROBE, timeout=90)
    except Exception as exc:  # noqa: BLE001
        report(False, f"docker exec {container}", str(exc), warn=True)
        return
    output = (completed.stdout or "") + (completed.stderr or "")
    result = parse_result(output)
    if result is None:
        report(False, "容器内插件探针输出", output[-500:] or "无输出")
        return

    config = result.get("config") if isinstance(result.get("config"), dict) else {}
    config_status = result.get("config_status")
    report(config_status == 200, "插件 /api/offline-market/config", str(config_status))
    if config_status == 200:
        print(
            f"        host_api_url={config.get('host_api_url') or '-'}"
            f"｜market_url={config.get('market_url') or '-'}"
            f"｜runtime_token={'有' if config.get('runtime_token_present') else '无'}"
        )
    else:
        return

    health = result.get("health") if isinstance(result.get("health"), dict) else {}
    report(
        health.get("ok") is True,
        "插件 → 宿主 API（/host-health）",
        str(health.get("message") or health.get("code") or health),
    )
    if health.get("ok") is True:
        print(
            f"        host_api_url={health.get('host_api_url') or '-'}"
            f"｜agents={health.get('agent_count')}"
        )
    else:
        print("        修复：确认 runtime 容器内 127.0.0.1:${QWENPAW_PORT} 可访问，"
              "且 QWENPAW_RUNTIME_INTERNAL_TOKEN 已注入（Hub 默认会注入）")


RUNTIME_PROBE = r'''
import json, os, urllib.error, urllib.request

port = os.environ.get("QWENPAW_PORT", "8088") or "8088"
base = "http://127.0.0.1:" + port
token = os.environ.get("QWENPAW_RUNTIME_INTERNAL_TOKEN", "")

def get(path):
    headers = {"accept": "application/json", "x-qwenpaw-runtime-token": token}
    request = urllib.request.Request(base + path, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as exc:
        raw = exc.read()
        try:
            payload = json.loads(raw) if raw else {}
        except Exception:
            payload = {"raw": raw.decode("utf-8", "replace")}
        return exc.code, payload
    except Exception as exc:
        return 0, {"error": repr(exc)}

config_status, config = get("/api/offline-market/config")
health_status, health = get("/api/offline-market/host-health")
print(
    "__RUNTIME_PROBE__"
    + json.dumps(
        {
            "config_status": config_status,
            "config": config,
            "health_status": health_status,
            "health": health,
        },
        ensure_ascii=False,
    )
)
'''


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
    parser.add_argument(
        "--public-url",
        help="后端的 MARKET_PUBLIC_URL（默认同 --market-url）：runtime 必须与它逐字符相同",
    )
    parser.add_argument(
        "--runtime-container",
        help="租户 runtime 的容器名（可选）：从容器内部探后端可达性与插件→宿主 API",
    )
    parser.add_argument("--qwenpaw", help="qwenpaw 可执行文件路径（可选）")
    args = parser.parse_args()

    public_url = (args.public_url or args.market_url).rstrip("/")
    print("离线技能市场 preflight")
    check_market(args.market_url, args.admin, args.admin_password)
    check_public_url(args.market_url, public_url)
    configs: list[Path] = []
    if args.runtimes_dir:
        root = Path(args.runtimes_dir)
        configs = sorted(root.rglob("config.json")) if root.is_dir() else []
    check_scan_dirs(configs)
    runtime_url = check_runtime_urls(public_url, configs)
    check_runtime_container(args.runtime_container, runtime_url or public_url)
    check_runtime_agent_api(args.runtime_container)
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
