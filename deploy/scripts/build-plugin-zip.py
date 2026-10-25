#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""打一个可在 Linux x64 上离线安装的 QwenPaw 插件 zip。

插件本身是纯 Python（stdlib，无编译产物、无 requirements.txt），所以这个 zip
是平台无关的；文件名里的 linux-x86_64 只是标明目标机器，和后端二进制包
（offline-market-backend-<ver>-linux-x86_64）保持同一套命名。

产物：
    dist/offline-market-plugin-<version>-linux-x86_64.zip
    dist/offline-market-plugin-<version>-linux-x86_64.zip.sha256

zip 结构（QwenPaw 2.2.2 的 `POST /api/plugins/upload` 认根目录或唯一一层
子目录里的 plugin.json，这里用后者，解压后目录名就是插件 id）：

    offline-market/
      plugin.json
      backend.py
      README.md
      frontend/market.js
      offline_market/*.py

用法：
    python3 deploy/scripts/build-plugin-zip.py
    python3 deploy/scripts/build-plugin-zip.py --plugin-dir plugin --out dist

装法（App 未运行时 qwenpaw 不会自己解压本地 zip，必须先解压）：

    unzip offline-market-plugin-<ver>-linux-x86_64.zip
    qwenpaw plugin install ./offline-market --force

App 正在运行时也可以直接 `qwenpaw plugin install <zip>`（走热安装 API）。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import zipfile
from pathlib import Path, PurePosixPath
from typing import NoReturn

# 不进包的：宿主不需要，且会把本地环境的缓存/字节码带给内网机器。
SKIP_DIRS = {"__pycache__", ".ruff_cache", ".pytest_cache", ".mypy_cache", "tests"}
SKIP_FILES = {".DS_Store", "Thumbs.db", "desktop.ini"}
SKIP_SUFFIXES = {".pyc", ".pyo", ".log"}

# Windows 检出是 CRLF、CI 检出是 LF；统一成 LF，同一个 commit 在哪台机器
# 打包都得到同一个 sha256（也给 Linux 目标机器省掉没意义的 \r）。
TEXT_SUFFIXES = {".py", ".js", ".md", ".json", ".txt", ".sh", ".yml", ".yaml"}

# 固定时间戳 + 固定权限 + 统一 LF：同一个 commit 在同一套 Python/zlib 上
# 重复打包 sha256 一致，且 Windows 检出的 CRLF 不会混进 Linux 包
# （跨 Python/zlib 版本压缩结果不同，不保证字节级一致）。
FIXED_DATE = (1980, 1, 1, 0, 0, 0)
FILE_MODE = 0o644


def fail(message: str) -> NoReturn:
    print(f"错误：{message}", file=sys.stderr)
    raise SystemExit(1)


def collect(plugin_dir: Path) -> list[tuple[PurePosixPath, Path]]:
    """返回 [(zip 内相对路径, 源文件路径)]。

    只写文件项、不写目录项：解压时目录自动创建，且 PowerShell 的
    Expand-Archive 遇到重复的目录项会报错。
    """
    entries: list[tuple[PurePosixPath, Path]] = []

    def walk(directory: Path, prefix: PurePosixPath) -> None:
        for path in sorted(directory.iterdir(), key=lambda p: p.name):
            if path.is_symlink():
                fail(f"插件目录里不允许符号链接：{path}")
            rel = prefix / path.name
            if path.is_dir():
                if path.name in SKIP_DIRS:
                    continue
                walk(path, rel)
            elif path.is_file():
                if path.name in SKIP_FILES or path.suffix in SKIP_SUFFIXES:
                    continue
                entries.append((rel, path))

    walk(plugin_dir, PurePosixPath(""))
    return entries


def validate(plugin_dir: Path) -> dict:
    manifest_path = plugin_dir / "plugin.json"
    if not manifest_path.is_file():
        fail(f"找不到 {manifest_path}")

    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        fail(f"plugin.json 不是合法 JSON：{exc}")

    for field in ("id", "name", "version"):
        if not manifest.get(field):
            fail(f"plugin.json 缺字段：{field}")

    entry = manifest.get("entry") or {}
    if not entry.get("backend"):
        fail("plugin.json 缺 entry.backend（宿主加载插件需要）")

    # 宿主的校验就是"这些文件必须存在"；早失败好过装到内网才发现。
    for name in entry.values():
        target = (plugin_dir / name).resolve()
        if not target.is_file():
            fail(f"plugin.json 指向的入口文件不存在：{name}")

    return manifest


def verify_zip(zip_path: Path, plugin_id: str, manifest: dict) -> None:
    """重新打开写好的包，按宿主的规则确认它能被认出来。"""
    with zipfile.ZipFile(zip_path) as archive:
        bad = archive.testzip()
        if bad is not None:
            fail(f"zip 校验失败（CRC 不匹配）：{bad}")

        names = archive.namelist()
        if any(n.startswith("/") or ".." in PurePosixPath(n).parts for n in names):
            fail("zip 里有绝对路径或 .. 路径")

        roots = {PurePosixPath(n).parts[0] for n in names if n}
        if roots != {plugin_id}:
            fail(f"zip 顶层目录必须只有 {plugin_id}/，实际是 {sorted(roots)}")

        manifest_in_zip = f"{plugin_id}/plugin.json"
        if manifest_in_zip not in names:
            fail(f"zip 里缺 {manifest_in_zip}（QwenPaw 只认根目录或顶层子目录）")

        for name in (manifest.get("entry") or {}).values():
            if f"{plugin_id}/{name}" not in names:
                fail(f"zip 里缺入口文件 {plugin_id}/{name}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def payload(path: Path) -> bytes:
    data = path.read_bytes()
    if path.suffix in TEXT_SUFFIXES:
        data = data.replace(b"\r\n", b"\n")
    return data


def build(plugin_dir: Path, out_dir: Path) -> Path:
    manifest = validate(plugin_dir)
    plugin_id = manifest["id"]
    version = manifest["version"]

    entries = collect(plugin_dir)
    if not entries:
        fail(f"{plugin_dir} 里没有可打包的文件")

    out_dir.mkdir(parents=True, exist_ok=True)
    zip_path = out_dir / f"offline-market-plugin-{version}-linux-x86_64.zip"

    with zipfile.ZipFile(
        zip_path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9
    ) as archive:
        for rel, source in entries:
            info = zipfile.ZipInfo(f"{plugin_id}/{rel.as_posix()}", FIXED_DATE)
            info.create_system = 3  # unix，权限位才有意义
            info.external_attr = FILE_MODE << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, payload(source))

    verify_zip(zip_path, plugin_id, manifest)

    digest = sha256(zip_path)
    sha_path = zip_path.with_name(zip_path.name + ".sha256")
    sha_path.write_text(f"{digest}  {zip_path.name}\n", encoding="utf-8")

    print(f"插件：{manifest['name']} ({plugin_id}) v{version}")
    print(f"文件：{len(entries)} 个，{zip_path.stat().st_size} 字节")
    print(f"产物：{zip_path}")
    print(f"校验：{sha_path}")
    print(f"sha256：{digest}")
    return zip_path


def main(argv: list[str] | None = None) -> int:
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--plugin-dir", default=root / "plugin", type=Path, help="插件源码目录（默认 plugin/）"
    )
    parser.add_argument(
        "--out", default=root / "dist", type=Path, help="输出目录（默认 dist/）"
    )
    args = parser.parse_args(argv)

    plugin_dir = args.plugin_dir.resolve()
    if not plugin_dir.is_dir():
        fail(f"插件目录不存在：{plugin_dir}")

    build(plugin_dir, args.out.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
