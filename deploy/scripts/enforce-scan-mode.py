#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把每个 QwenPaw 租户 runtime 的技能扫描模式强制成 block。

为什么需要这个脚本：QwenPaw 默认 `security.skill_scanner.mode = "warn"`，
而 warn 模式下 CRITICAL 级别的包**照样安装并启用**（实测，见
spikes/findings-2.2.2.md 结论 9）。插件接受 warn，但只告警不拦；想真正
拦住恶意包就部署成 block，这个脚本负责批量改。

为什么不能只设环境变量：Hub 的两个 provisioner 都用
`system_environment()`（`src/qwenpaw/utils/runtime_environment.py`）构造
runtime 环境，它只继承 LANG/LC_*；而 credentials 投影又被
`_CONTROL_PREFIXES` 里的 `QWENPAW_` 前缀挡住。所以
`QWENPAW_SKILL_SCAN_MODE` **无法**从 hub 注入，只能落到每个 runtime 的
config.json 里。

用法：
    python3 enforce-scan-mode.py --runtimes-dir /var/lib/qwenpaw-hub/runtimes --dry-run
    python3 enforce-scan-mode.py --runtimes-dir /var/lib/qwenpaw-hub/runtimes

    # 也可以直接指单个 config.json
    python3 enforce-scan-mode.py --config /home/tenant/.qwenpaw/config.json

docker provisioner 的 runtime 目录在容器/卷里，可在宿主上挂载卷后运行本脚本，
或用 `docker run --rm -v <volume>:/runtimes python:3 python /runtimes/...`。
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
from pathlib import Path

MODE = "block"


def patch(path: Path, dry_run: bool) -> str:
    try:
        payload = json.loads(path.read_text(encoding="utf-8") or "{}")
    except json.JSONDecodeError as exc:
        return f"SKIP  {path}（不是合法 JSON：{exc}）"
    if not isinstance(payload, dict):
        return f"SKIP  {path}（顶层不是对象）"

    security = payload.setdefault("security", {})
    if not isinstance(security, dict):
        return f"SKIP  {path}（security 不是对象）"
    scanner = security.setdefault("skill_scanner", {})
    if not isinstance(scanner, dict):
        return f"SKIP  {path}（security.skill_scanner 不是对象）"

    previous = scanner.get("mode")
    if previous == MODE:
        return f"OK    {path}（已是 {MODE}）"
    scanner["mode"] = MODE
    if dry_run:
        return f"WOULD {path}（{previous!r} -> {MODE!r}）"

    shutil.copy2(path, path.with_suffix(path.suffix + ".bak"))
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    tmp.replace(path)
    return f"FIXED {path}（{previous!r} -> {MODE!r}，备份在 *.bak）"


def discover(target: Path) -> list[Path]:
    if target.is_file():
        return [target]
    found = sorted(target.rglob("config.json"))
    return [path for path in found if ".bak" not in path.name]


def main() -> int:
    parser = argparse.ArgumentParser(description="force QwenPaw skill scanner to block mode")
    parser.add_argument("--runtimes-dir", help="hub 的 runtimes 目录（递归找 config.json）")
    parser.add_argument("--config", action="append", default=[], help="直接指定 config.json，可重复")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    targets: list[Path] = [Path(path) for path in args.config]
    if args.runtimes_dir:
        targets.extend(discover(Path(args.runtimes_dir)))
    if not targets:
        parser.error("至少给一个 --runtimes-dir 或 --config")

    fixed = 0
    ok = 0
    for path in targets:
        if not path.exists():
            print(f"MISS  {path}")
            continue
        message = patch(path, args.dry_run)
        print(message)
        if message.startswith(("FIXED", "WOULD")):
            fixed += 1
        elif message.startswith("OK"):
            ok += 1

    print(f"\n{len(targets)} 个文件：{ok} 已合规，{fixed} 需修改/已修改"
          + ("（dry-run，未写入）" if args.dry_run else ""))
    print("提醒：改完要重启对应租户 runtime 才生效。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
