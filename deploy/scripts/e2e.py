#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""离线技能市场端到端验收（stdlib only，无第三方依赖）。

两个阶段：

  A. 后端自检（自带后端进程，用临时库，不碰生产数据）
     启动 → 发布 → 列表 → 下载 → 私有隔离 403 → 版本不可变 409 →
     管理员登录 → 下架 → 410 → 恢复 → 审计留痕

  B. 线上部署校验（针对已经跑起来的 QwenPaw + 本插件）
     provider 可用 → 宿主搜索 → 宿主安装任务完成 → 管理员下架 →
     搜索消失 + 重装 410

用法：
    # 只跑 A（离线机器上最常用）
    python3 e2e.py --binary ./offline-market-backend

    # A + B（需要 QwenPaw app 已在运行，且插件已装、env 已设）
    python3 e2e.py --binary ./offline-market-backend \
        --app-url http://127.0.0.1:8123 --tenant tenant-b

退出码：0 = 全通过。
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

PASS, FAIL = "\033[32mPASS\033[0m", "\033[31mFAIL\033[0m"
results: list[tuple[bool, str, str]] = []


def check(ok: bool, name: str, detail: str = "") -> bool:
    results.append((ok, name, detail))
    print(f"  [{PASS if ok else FAIL}] {name}" + (f" — {detail}" if detail and not ok else ""))
    return ok


def http(method: str, url: str, body=None, headers=None, timeout=15):
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
            payload = {"raw": raw.decode("utf-8", "replace")}
        return exc.code, payload, exc.headers
    except urllib.error.URLError as exc:
        return 0, {"error": str(exc.reason)}, {}


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_http(url: str, timeout: float = 30.0) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        status, _, _ = http("GET", url, timeout=2)
        if status == 200:
            return True
        time.sleep(0.3)
    return False


# --------------------------------------------------------------------- phase A

MANIFEST = "---\nname: %s\ndescription: e2e smoke skill\n---\n\n# %s\n"


def phase_a(binary: str, workdir: str) -> dict:
    print("\n[A] 后端自检（临时库）")
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    db = os.path.join(workdir, "audit.sqlite3")
    # 后端日志写文件而不是 PIPE：没人读 PIPE 时，写满管道缓冲区后端会卡死
    # （Windows 默认 4 KB，每条请求一行日志，phase A 二十来个请求就能写满）。
    log_path = os.path.join(workdir, "backend.log")
    env = dict(os.environ)
    env.update(
        {
            "MARKET_DB": db,
            "MARKET_BIND": f"127.0.0.1:{port}",
            "MARKET_PUBLIC_URL": base,
            "ADMIN_USER": "admin",
            "ADMIN_PASSWORD": "e2e-password",
        }
    )
    log_file = open(log_path, "w", encoding="utf-8")
    process = subprocess.Popen(
        [binary],
        env=env,
        stdout=log_file,
        stderr=subprocess.STDOUT,
    )
    try:
        if not check(wait_http(f"{base}/health"), "后端启动 /health"):
            return {"ok": False, "url": base}

        status, limits, _ = http("GET", f"{base}/api/v1/limits", headers={"x-tenant-id": "t-a"})
        check(status == 200 and limits.get("manifest") == "SKILL.md", "限额端点返回 manifest/limits")

        slug = f"e2e-{secrets.token_hex(3)}"
        status, body, _ = http(
            "POST",
            f"{base}/api/v1/skills",
            {
                "name": f"E2E {slug}",
                "slug": slug,
                "version": "1.0.0",
                "visibility": "public",
                "files": {"SKILL.md": MANIFEST % (slug, slug)},
            },
            {"x-tenant-id": "t-a"},
        )
        check(status == 201, "发布公开技能", f"{status} {body}")
        check(
            body.get("install_url") == f"{base}/skills/{slug}/1.0.0",
            "install_url 指向租户可达地址",
            str(body.get("install_url")),
        )

        status, body, _ = http(
            "POST",
            f"{base}/api/v1/skills",
            {
                "name": f"E2E {slug}",
                "slug": slug,
                "version": "1.0.0",
                "visibility": "public",
                "files": {"SKILL.md": MANIFEST % (slug, slug)},
            },
            {"x-tenant-id": "t-a"},
        )
        check(status == 409 and body.get("error", {}).get("code") == "version_exists", "版本不可变 → 409")

        status, body, _ = http(
            "POST",
            f"{base}/api/v1/skills",
            {
                "name": "bad",
                "slug": f"{slug}-bad",
                "version": "1.0.0",
                "visibility": "public",
                "files": {"SKILL.md": MANIFEST % ("bad", "bad"), "../escape.md": "x"},
            },
            {"x-tenant-id": "t-a"},
        )
        check(status == 400, "路径穿越被闸门拒绝 → 400", f"{status} {body}")

        status, body, _ = http(
            "POST",
            f"{base}/api/v1/skills",
            {
                "name": "nomf",
                "slug": f"{slug}-nomf",
                "version": "1.0.0",
                "visibility": "public",
                "files": {"SKILL.md": "# no frontmatter\n"},
            },
            {"x-tenant-id": "t-a"},
        )
        check(
            status == 400 and body.get("error", {}).get("code") == "invalid_manifest",
            "缺 frontmatter 被拒（否则宿主装不上）",
            f"{status} {body}",
        )

        status, listing, _ = http("GET", f"{base}/api/v1/skills?q={slug}", headers={"x-tenant-id": "t-b"})
        check(
            status == 200 and any(item["slug"] == slug for item in listing.get("items", [])),
            "其他租户能看到公开技能",
        )

        status, bundle, _ = http("GET", f"{base}/skills/{slug}/1.0.0", headers={"x-tenant-id": "t-b"})
        check(status == 200 and "SKILL.md" in bundle.get("files", {}), "下载 bundle 成功")

        private_slug = f"{slug}-priv"
        http(
            "POST",
            f"{base}/api/v1/skills",
            {
                "name": f"E2E {private_slug}",
                "slug": private_slug,
                "version": "1.0.0",
                "visibility": "tenant",
                "files": {"SKILL.md": MANIFEST % (private_slug, private_slug)},
            },
            {"x-tenant-id": "t-a"},
        )
        status, _, _ = http("GET", f"{base}/skills/{private_slug}/1.0.0", headers={"x-tenant-id": "t-b"})
        check(status == 403, "私有技能跨租户下载 → 403", str(status))
        status, listing, _ = http("GET", f"{base}/api/v1/skills", headers={"x-tenant-id": "t-b"})
        check(
            not any(item["slug"] == private_slug for item in listing.get("items", [])),
            "私有技能不出现在他人列表",
        )

        cookie = ""
        status, _, headers = http(
            "POST", f"{base}/admin/api/login", {"username": "admin", "password": "e2e-password"}
        )
        if status == 200:
            cookie = headers.get("set-cookie", "").split(";", 1)[0]
        check(status == 200 and bool(cookie), "管理员登录")

        status, _, _ = http("POST", f"{base}/admin/api/skills/{slug}/takedown", None, {"cookie": cookie})
        check(status == 200, "管理员下架")
        for tenant in ("t-a", "t-b"):
            status, _, _ = http("GET", f"{base}/skills/{slug}/1.0.0", headers={"x-tenant-id": tenant})
            check(status == 410, f"下架后 {tenant} 拿到 410", str(status))
        status, body, _ = http(
            "GET", f"{base}/api/v1/skills/lookup?names=E2E%20{slug}", headers={"x-tenant-id": "t-a"}
        )
        check(
            status == 200
            and (body.get("items") or [{}])[0].get("state") == "taken_down",
            "lookup 仍能查到（供「已安装」页显示已下架）",
            str(body)[:200],
        )
        status, _, _ = http("POST", f"{base}/api/v1/skills/{slug}/restore", None, {"x-tenant-id": "t-a"})
        check(status == 409, "属主不能自助恢复被下架技能 → 409", str(status))
        status, _, _ = http("POST", f"{base}/admin/api/skills/{slug}/restore", None, {"cookie": cookie})
        check(status == 200, "管理员恢复上架")

        status, audit, _ = http("GET", f"{base}/admin/api/audit?limit=50", headers={"cookie": cookie})
        actions = [item.get("action") for item in audit.get("items", [])]
        check(status == 200 and "takedown" in actions and "publish" in actions, "审计记录 publish/takedown")

        # 重启后数据仍在（SQLite 落盘）
        process.terminate()
        process.wait(timeout=15)
        log_file.close()
        log_file = open(log_path, "a", encoding="utf-8")
        process = subprocess.Popen(
            [binary],
            env=env,
            stdout=log_file,
            stderr=subprocess.STDOUT,
        )
        if wait_http(f"{base}/health"):
            status, _, _ = http("GET", f"{base}/skills/{slug}/1.0.0", headers={"x-tenant-id": "t-b"})
            check(status == 200, "重启后数据保留（SQLite 持久化）", str(status))
        else:
            check(False, "重启后后端未恢复")
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:  # pragma: no cover
            process.kill()
        log_file.close()

    if not all(ok for ok, _, _ in results):
        print("  后端日志（末尾 40 行）：")
        try:
            with open(log_path, encoding="utf-8", errors="replace") as handle:
                for line in handle.read().splitlines()[-40:]:
                    print(f"    {line}")
        except OSError:  # pragma: no cover
            pass

    return {"ok": all(ok for ok, _, _ in results), "url": base, "audit_db": db}


# --------------------------------------------------------------------- phase B

def phase_b(app_url: str, tenant: str, market_url: str, admin: tuple[str, str]) -> bool:
    print("\n[B] 线上部署校验（真实 QwenPaw + 本插件）")
    app = app_url.rstrip("/")
    started_ok = True

    status, providers, _ = http("GET", f"{app}/api/market/providers")
    ours = [p for p in providers if p.get("key") == "offline-market"] if status == 200 else []
    started_ok &= check(status == 200 and bool(ours), "宿主已加载 offline-market provider")
    if ours:
        started_ok &= check(
            ours[0].get("available") is True,
            "provider 可用（配置齐全且扫描为 block）",
            str(ours[0].get("reason")),
        )

    status, listing, _ = http("GET", f"{market_url}/api/v1/skills?limit=50", headers={"x-tenant-id": tenant})
    candidates = [item for item in listing.get("items", []) if item.get("latest_version")]
    if not candidates:
        return check(False, "市场里至少有一个可安装技能")
    target = candidates[0]

    status, search, _ = http(
        "POST",
        f"{app}/api/market/search",
        {"query": target["slug"], "provider_pages": {"offline-market": 1}, "limit": 10},
    )
    started_ok &= check(
        status == 200
        and any(item["slug"] == target["slug"] for item in search.get("results", []))
        and not search.get("errors"),
        "宿主搜索能搜到该技能",
        str(search)[:300],
    )

    status, task, _ = http(
        "POST",
        f"{app}/api/skills/hub/install/start",
        {
            "provider_key": "offline-market",
            "bundle_url": f"{market_url}/skills/{target['slug']}/{target['latest_version']}",
            "version": target["latest_version"],
            "enable": True,
            "target_name": f"e2e-{secrets.token_hex(2)}",
        },
    )
    ok_install = False
    if status == 200:
        task_id = task.get("task_id")
        for _ in range(60):
            time.sleep(0.5)
            status, state, _ = http("GET", f"{app}/api/skills/hub/install/status/{task_id}")
            if state.get("status") in {"completed", "failed", "cancelled"}:
                ok_install = state.get("status") == "completed" and (
                    (state.get("result") or {}).get("installed_from") == "offline-market"
                )
                detail = str(state.get("error") or state.get("status"))
                break
        else:
            detail = "timeout"
    else:
        detail = str(task)
    started_ok &= check(ok_install, "宿主安装任务完成且来源正确", detail)

    cookie = ""
    status, _, headers = http("POST", f"{market_url}/admin/api/login", {"username": admin[0], "password": admin[1]})
    if status != 200:
        return check(False, "管理员登录（phase B 需要市场管理员口令）")
    cookie = headers.get("set-cookie", "").split(";", 1)[0]

    http("POST", f"{market_url}/admin/api/skills/{target['slug']}/takedown", None, {"cookie": cookie})
    status, search_after, _ = http(
        "POST",
        f"{app}/api/market/search",
        {"query": target["slug"], "provider_pages": {"offline-market": 1}, "limit": 10},
    )
    started_ok &= check(
        status == 200
        and not any(item["slug"] == target["slug"] for item in search_after.get("results", [])),
        "下架后宿主搜索不再出现",
    )

    status, task, _ = http(
        "POST",
        f"{app}/api/skills/hub/install/start",
        {
            "provider_key": "offline-market",
            "bundle_url": f"{market_url}/skills/{target['slug']}/{target['latest_version']}",
            "version": target["latest_version"],
            "target_name": f"e2e-takedown-{secrets.token_hex(2)}",
        },
    )
    failed_410 = False
    if status == 200:
        task_id = task.get("task_id")
        for _ in range(40):
            time.sleep(0.5)
            _, state, _ = http("GET", f"{app}/api/skills/hub/install/status/{task_id}")
            if state.get("status") in {"completed", "failed", "cancelled"}:
                error = str(state.get("error") or "")
                failed_410 = state.get("status") == "failed" and (
                    "410" in error or "taken down" in error or "no longer be downloaded" in error
                )
                break
    started_ok &= check(failed_410, "下架后再装 → 410 Gone")

    http("POST", f"{market_url}/admin/api/skills/{target['slug']}/restore", None, {"cookie": cookie})
    return started_ok


def main() -> int:
    parser = argparse.ArgumentParser(description="offline skill market e2e")
    parser.add_argument("--binary", help="后端可执行文件（phase A）")
    parser.add_argument("--app-url", help="已运行的 QwenPaw 控制台（phase B）")
    parser.add_argument("--market-url", help="市场后端地址（phase B），默认同 --app-url 的部署值")
    parser.add_argument("--tenant", default="e2e-tenant", help="phase B 使用的租户 id")
    parser.add_argument("--admin", default="admin", help="市场管理员用户名")
    parser.add_argument("--admin-password", default=os.environ.get("ADMIN_PASSWORD", ""))
    parser.add_argument("--skip-a", action="store_true")
    parser.add_argument("--skip-b", action="store_true")
    args = parser.parse_args()

    ok = True
    workdir = tempfile.mkdtemp(prefix="market-e2e-")
    try:
        if not args.skip_a:
            if not args.binary:
                print("缺少 --binary，无法运行 phase A")
                return 2
            result = phase_a(os.path.abspath(args.binary), workdir)
            ok &= bool(result.get("ok"))
            if not args.market_url:
                args.market_url = result.get("url")
        if not args.skip_b:
            if not args.app_url:
                print("\n[B] 跳过（未提供 --app-url）")
            else:
                ok &= phase_b(
                    args.app_url,
                    args.tenant,
                    (args.market_url or "").rstrip("/"),
                    (args.admin, args.admin_password or "e2e-password"),
                )
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    passed = sum(1 for ok_item, _, _ in results if ok_item)
    total = len(results)
    print(f"\n{passed}/{total} 检查通过")
    failures = [name for ok_item, name, _ in results if not ok_item]
    if failures:
        print("失败项：")
        for name in failures:
            print(f"  - {name}")
    return 0 if ok and passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
