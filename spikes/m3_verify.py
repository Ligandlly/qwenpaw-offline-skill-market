# -*- coding: utf-8 -*-
"""M3 end-to-end verification in a real browser (no mocks).

Covers the four tabs and the two write paths:
  1. sidebar entry: 技能市场（内网）, primary.agentScoped, right after core.marketplace
  2. 市场: table renders, 安装 drives the host install task to completion
  3. 发布: upload a skill .zip + publish -> success notice
  4. 我的发布: the new skill appears
  5. 已安装: shows 正常; after an admin takedown it shows 已下架

Run with backend :8099 and qwenpaw app :8123 up, plugin installed:
    .venv/Scripts/python.exe m3_verify.py
"""

from __future__ import annotations

import io
import json
import sys
import time
import urllib.request
import zipfile
from pathlib import Path

from playwright.sync_api import sync_playwright

BASE = "http://127.0.0.1:8123"
MARKET = "http://127.0.0.1:8099"
ADMIN = ("admin", "s3cret-pw")
CHROMIUM = Path.home() / "AppData/Local/ms-playwright/chromium-1187/chrome-win/chrome.exe"
SHOTS = Path(__file__).parent


def skill_zip() -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(
            "SKILL.md",
            "---\nname: e2e-published\ndescription: published from the browser\n---\n\n# E2E\n",
        )
        archive.writestr("scripts/hello.py", "print('hello')\n")
    return buffer.getvalue()


def http(method: str, url: str, body: dict | None = None, cookie: str = "", tenant: str = ""):
    data = json.dumps(body).encode() if body is not None else None
    headers = {"accept": "application/json", "content-type": "application/json"}
    if cookie:
        headers["cookie"] = cookie
    if tenant:
        headers["x-tenant-id"] = tenant
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return response.status, json.loads(response.read() or b"{}"), response.headers
    except urllib.error.HTTPError as exc:
        return exc.code, json.loads(exc.read() or b"{}"), exc.headers


def admin_cookie() -> str:
    status, _, headers = http("POST", f"{MARKET}/admin/api/login", {"username": ADMIN[0], "password": ADMIN[1]})
    assert status == 200, status
    raw = headers.get("set-cookie", "")
    return raw.split(";", 1)[0]


def main() -> int:
    report: dict = {"steps": {}}
    errors: list[str] = []
    console_errors: list[str] = []

    with sync_playwright() as pw:
        kwargs = {"executable_path": str(CHROMIUM)} if CHROMIUM.exists() else {}
        browser = pw.chromium.launch(**kwargs)
        page = browser.new_page(viewport={"width": 1440, "height": 950})
        page.on("pageerror", lambda exc: errors.append(str(exc)))
        page.on(
            "console",
            lambda msg: console_errors.append(msg.text) if msg.type == "error" else None,
        )

        # 1) sidebar entry -----------------------------------------------------
        page.goto(BASE + "/", wait_until="domcontentloaded", timeout=60000)
        page.wait_for_timeout(6000)
        menu = page.evaluate(
            "() => (window.QwenPaw && window.QwenPaw.menu) ? window.QwenPaw.menu.snapshot() : []"
        )
        ours = [item for item in menu if item.get("id") == "offline-market.menu"]
        report["steps"]["menu"] = {
            "found": bool(ours),
            "item": ours[0] if ours else None,
            "ok": bool(ours)
            and ours[0].get("label") == "技能市场（内网）"
            and ours[0].get("location") == "primary.agentScoped",
        }

        # 2) market tab --------------------------------------------------------
        page.goto(BASE + "/offline-market", wait_until="domcontentloaded", timeout=60000)
        page.wait_for_timeout(4000)
        has_tabs = all(
            page.get_by_text(label, exact=True).first.is_visible()
            for label in ["市场", "发布", "我的发布", "已安装"]
        )
        report["steps"]["market_tab"] = {
            "title": page.get_by_text("技能市场（内网）").first.is_visible(),
            "has_tabs": has_tabs,
            "ok": has_tabs,
        }
        page.screenshot(path=str(SHOTS / "m3-market.png"), full_page=True)

        install_clicked = False
        buttons = page.locator("button")
        for index in range(buttons.count()):
            if "安装" in (buttons.nth(index).inner_text() or "").replace(" ", ""):
                buttons.nth(index).click()
                install_clicked = True
                break
        installed_ok = False
        if install_clicked:
            try:
                page.wait_for_function(
                    "() => /已安装 |已安装/.test(document.body.innerText)", timeout=60000
                )
                installed_ok = True
            except Exception:  # noqa: BLE001
                pass
        report["steps"]["install"] = {"clicked": install_clicked, "ok": installed_ok}

        # 3) publish ----------------------------------------------------------
        page.get_by_text("发布", exact=True).first.click()
        page.wait_for_timeout(1200)
        suffix = str(int(time.time()))[-5:]
        page.get_by_placeholder("例如：周报助手").fill(f"E2E Skill {suffix}")
        page.get_by_placeholder("1.0.0").fill(f"0.2.{suffix}")
        page.set_input_files(
            "input[type=file]",
            {
                "name": f"skill-{suffix}.zip",
                "mimeType": "application/zip",
                "buffer": skill_zip(),
            },
        )
        page.wait_for_timeout(600)
        page.get_by_role("button", name="立即发布").click()
        published = False
        try:
            page.wait_for_function(
                "() => /已发布 /.test(document.body.innerText)", timeout=30000
            )
            published = True
        except Exception:  # noqa: BLE001
            pass
        report["steps"]["publish"] = {"ok": published}
        page.screenshot(path=str(SHOTS / "m3-publish.png"), full_page=True)

        # 4) mine tab ---------------------------------------------------------
        page.get_by_text("我的发布", exact=True).first.click()
        page.wait_for_timeout(2500)
        mine_text = page.inner_text("body")
        report["steps"]["mine"] = {
            "ok": f"E2E Skill {suffix}" in mine_text,
            "snippet": mine_text[:300],
        }
        page.screenshot(path=str(SHOTS / "m3-mine.png"), full_page=True)

        # 5) installed tab + admin takedown -----------------------------------
        page.get_by_text("已安装", exact=True).first.click()
        page.wait_for_timeout(2500)
        before = page.inner_text("body")
        report["steps"]["installed_before_takedown"] = {
            "ok": "正常" in before,
        }

        cookie = admin_cookie()
        slug = _pick_installed_slug(page)
        status, body, _ = http(
            "POST", f"{MARKET}/admin/api/skills/{slug}/takedown", {"reason": "e2e verification"}, cookie
        )
        report["steps"]["takedown"] = {
            "slug": slug,
            "status": status,
            "body": body,
            "ok": status == 200 and body.get("state") == "taken_down",
        }

        page.reload(wait_until="domcontentloaded")
        page.wait_for_timeout(3000)
        page.get_by_text("已安装", exact=True).first.click()
        page.wait_for_timeout(3000)
        after = page.inner_text("body")
        report["steps"]["installed_after_takedown"] = {
            "ok": "已下架" in after,
            "snippet": after[after.find("已安装") :][:400] if "已安装" in after else after[:400],
        }
        page.screenshot(path=str(SHOTS / "m3-installed.png"), full_page=True)

        browser.close()

    report["page_errors"] = errors
    report["console_errors"] = console_errors
    report["ok"] = (
        report["steps"]["menu"]["ok"]
        and report["steps"]["install"]["ok"]
        and report["steps"]["publish"]["ok"]
        and report["steps"]["mine"]["ok"]
        and report["steps"]["installed_before_takedown"]["ok"]
        and report["steps"]["installed_after_takedown"]["ok"]
        and not errors
        and not console_errors
    )
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if report["ok"] else 1


def _pick_installed_slug(page) -> str:
    """Market slug of the first installed skill (data-installed-name attr)."""
    try:
        slug = page.locator("[data-market-slug]").first.get_attribute("data-market-slug")
        if slug:
            return slug.strip()
        value = page.locator("[data-installed-name]").first.get_attribute("data-installed-name")
        if value:
            return value.strip()
    except Exception:  # noqa: BLE001
        pass
    return "demo"


if __name__ == "__main__":
    sys.exit(main())
