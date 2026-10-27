# -*- coding: utf-8 -*-
"""Verify the frontend entry actually mounts in the real console.

Checks, in order:
  1. the console boots and `window.QwenPaw.host` exposes React
  2. our menu item is in the sidebar registry
  3. our route renders at /offline-market and shows live market data
  4. no page errors / console errors from our bundle

Run (with qwenpaw app on :8123):
    .venv/Scripts/python.exe v_frontend.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from playwright.sync_api import sync_playwright

BASE = "http://127.0.0.1:8123"
# The bundled playwright package wants a newer chromium than the one QwenPaw
# already downloaded; reuse the installed build instead of a 150 MB download.
CHROMIUM = Path.home() / "AppData/Local/ms-playwright/chromium-1187/chrome-win/chrome.exe"
SHOT = Path(__file__).with_name("m3-verify.png")


def main() -> int:
    logs: list[str] = []
    errors: list[str] = []
    report: dict = {}

    with sync_playwright() as pw:
        launch_kwargs = {"executable_path": str(CHROMIUM)} if CHROMIUM.exists() else {}
        browser = pw.chromium.launch(**launch_kwargs)
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        page.on(
            "console",
            lambda msg: logs.append(f"{msg.type}: {msg.text}")
            if msg.type in {"error", "warning", "info"}
            else None,
        )
        page.on("pageerror", lambda exc: errors.append(str(exc)))

        page.goto(BASE + "/", wait_until="domcontentloaded", timeout=60000)
        page.wait_for_timeout(6000)

        report["has_host"] = page.evaluate(
            "() => Boolean(window.QwenPaw && window.QwenPaw.host && window.QwenPaw.host.React)"
        )
        report["menu"] = page.evaluate(
            "() => (window.QwenPaw && window.QwenPaw.menu) ? window.QwenPaw.menu.snapshot() : null"
        )
        report["route_ids"] = page.evaluate(
            "() => (window.QwenPaw && window.QwenPaw.route) ? null : undefined"
        )

        ours = [
            item
            for item in (report["menu"] or [])
            if "offline-market" in json.dumps(item)
        ]
        report["our_menu_items"] = ours

        page.goto(BASE + "/offline-market", wait_until="domcontentloaded", timeout=60000)
        page.wait_for_timeout(4000)
        report["page_text"] = page.inner_text("body")[:1200]
        page.screenshot(path=str(SHOT), full_page=True)

        # 5) click 安装 in the market page and wait for the host's task to finish
        clicked = False
        buttons = page.locator("button")
        for index in range(buttons.count()):
            text = (buttons.nth(index).inner_text() or "").replace(" ", "")
            if "安装" in text:
                buttons.nth(index).click()
                clicked = True
                break
        report["install_clicked"] = clicked
        if clicked:
            try:
                page.wait_for_function(
                    "() => /已安装|安装失败|已下架|不可/.test(document.body.innerText)",
                    timeout=60000,
                )
            except Exception as exc:  # noqa: BLE001
                report["install_wait_error"] = str(exc)[:200]
            report["page_text_after_install"] = page.inner_text("body")[:1500]
            page.screenshot(path=str(SHOT), full_page=True)

        report["console_errors"] = [line for line in logs if line.startswith("error")]
        report["page_errors"] = errors
        browser.close()

    ok = (
        report["has_host"]
        and bool(ours)
        and "内网市场" in report["page_text"]
        and report.get("install_clicked")
        and "已安装" in (report.get("page_text_after_install") or "")
        and not report["page_errors"]
        and not report["console_errors"]
    )
    report["ok"] = ok
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
