# -*- coding: utf-8 -*-
"""Turn an uploaded ``.zip`` into the backend's bundle map.

Files are kept as raw bytes here: skills legitimately ship ``.docx``/``.pptx``
and images. :func:`wire_files` converts them to the backend's JSON contract —
UTF-8 files as plain strings, everything else as ``{"base64": "..."}``.

This module is deliberately *not* the distribution gate: the backend still
enforces the real, configurable limits (file count, per-file and total size,
path depth). The caps below only stop a small archive from exploding inside the
tenant runtime before it ever reaches the backend.

The plugin is stdlib-only by policy (plugins run in the host's venv), hence
``zipfile`` and no yaml/zip dependencies.
"""

from __future__ import annotations

import base64
import io
import stat
import zipfile

MANIFEST = "SKILL.md"

# Hard safety caps, not the distribution limits (those live in the backend and
# are configurable via env). 16 MiB is 8x the backend default of 2 MiB, so a
# raised backend limit still works while a zip bomb does not.
MAX_UPLOAD_BYTES = 16 * 1024 * 1024
MAX_TOTAL_BYTES = 16 * 1024 * 1024
MAX_FILE_BYTES = 4 * 1024 * 1024
MAX_FILES = 1000

# Junk that macOS/Windows add and nobody wants in a skill version.
IGNORED_BASENAMES = frozenset({".DS_Store", "Thumbs.db", "desktop.ini"})
IGNORED_PREFIX = "__MACOSX/"


class BundleError(ValueError):
    """The archive cannot be turned into a bundle. ``code`` mirrors the backend."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def extract_zip(data: bytes) -> dict[str, bytes]:
    """``.zip`` bytes -> ``{path: raw bytes}``. Raises :class:`BundleError`."""
    if not data:
        raise BundleError("empty_upload", "压缩包是空的")
    if len(data) > MAX_UPLOAD_BYTES:
        raise BundleError(
            "upload_too_large", f"压缩包超过 {MAX_UPLOAD_BYTES} 字节上限"
        )

    try:
        archive = zipfile.ZipFile(io.BytesIO(data))
    except (zipfile.BadZipFile, OSError) as exc:
        raise BundleError("invalid_zip", "不是有效的 zip 压缩包") from exc

    try:
        with archive:
            entries = _read_entries(archive)
    except (zipfile.BadZipFile, EOFError) as exc:
        raise BundleError("invalid_zip", "压缩包已损坏或不完整") from exc

    files = _strip_single_root(entries)
    if MANIFEST not in files:
        raise BundleError(
            "missing_manifest",
            f"压缩包根目录必须有 {MANIFEST}；"
            "如果技能放在一个文件夹里，请压缩该文件夹里的内容"
            "（程序会自动去掉唯一的顶层目录）",
        )
    return files


# --------------------------------------------------------------------- inner ---


def wire_files(files: dict[str, bytes]) -> dict[str, str | dict[str, str]]:
    """Raw files -> the backend's JSON map.

    JSON cannot carry bytes, so the market accepts ``{"base64": "..."}`` for
    payloads that are not valid UTF-8. Text is sent verbatim (with the BOM
    stripped, as before), which keeps the common case readable and backwards
    compatible with every existing API client.
    """
    out: dict[str, str | dict[str, str]] = {}
    for path, raw in files.items():
        try:
            out[path] = raw.decode("utf-8-sig")
        except UnicodeDecodeError:
            out[path] = {"base64": base64.b64encode(raw).decode("ascii")}
    return out


def _read_entries(archive: zipfile.ZipFile) -> list[tuple[str, bytes]]:
    entries: list[tuple[str, bytes]] = []
    seen: set[str] = set()
    total = 0
    for info in archive.infolist():
        path = _entry_path(info)
        if path is None:  # directory or OS junk
            continue
        if path in seen:
            raise BundleError("duplicate_entry", f"{path}: 压缩包里有重名条目")
        seen.add(path)
        if len(seen) > MAX_FILES:
            raise BundleError("too_many_files", f"压缩包文件数超过 {MAX_FILES}")

        raw = _read_entry(archive, info, path)
        total += len(raw)
        if total > MAX_TOTAL_BYTES:
            raise BundleError(
                "bundle_too_large", f"解压后总大小超过 {MAX_TOTAL_BYTES} 字节上限"
            )
        entries.append((path, raw))
    return entries


def _entry_path(info: zipfile.ZipInfo) -> str | None:
    """Validated POSIX path, or ``None`` for entries we silently drop."""
    name = info.filename.replace("\\", "/")
    if info.is_dir() or name.endswith("/"):
        return None
    basename = name.rsplit("/", 1)[-1]
    if name.startswith(IGNORED_PREFIX) or basename in IGNORED_BASENAMES:
        return None

    if name.startswith("/") or ":" in name:
        raise BundleError("absolute_path", f"{name}: 不允许绝对路径或盘符")
    if any(ord(char) < 32 for char in name):
        raise BundleError("invalid_path", f"{name!r}: 路径含控制字符")

    segments = name.split("/")
    if any(segment in ("", ".", "..") for segment in segments):
        raise BundleError("traversal", f"{name}: 不允许空路径段、'.' 或 '..'")
    return "/".join(segments)


def _read_entry(archive: zipfile.ZipFile, info: zipfile.ZipInfo, path: str) -> bytes:
    if info.flag_bits & 0x1:
        raise BundleError("encrypted_entry", f"{path}: 不支持加密的条目")
    if stat.S_ISLNK((info.external_attr >> 16) & 0xFFFF):
        raise BundleError("symlink_entry", f"{path}: 不支持符号链接")
    if info.file_size > MAX_FILE_BYTES:
        raise BundleError("file_too_large", f"{path}: 超过 {MAX_FILE_BYTES} 字节上限")

    try:
        # Read capped: the size in the header is attacker-controlled.
        with archive.open(info) as handle:
            raw = handle.read(MAX_FILE_BYTES + 1)
    except (zipfile.BadZipFile, RuntimeError, NotImplementedError, OSError) as exc:
        raise BundleError("bad_entry", f"{path}: 解压失败（{exc}）") from exc
    if len(raw) > MAX_FILE_BYTES:
        raise BundleError("file_too_large", f"{path}: 超过 {MAX_FILE_BYTES} 字节上限")

    # Keep bytes as-is; wire_files() decides text vs base64. In particular this
    # must not reject a .docx just because it is not UTF-8.
    return raw


def _strip_single_root(entries: list[tuple[str, bytes]]) -> dict[str, bytes]:
    """``my-skill/SKILL.md`` -> ``SKILL.md`` when the folder is the only root.

    Zipping a folder is the natural thing to do on every OS, and it produces one
    extra level that the backend (correctly) rejects. Only strip when every
    entry lives under the same top-level folder, so a genuinely nested bundle is
    left alone and fails with an accurate missing-manifest error.
    """
    if not entries or any(path == MANIFEST for path, _ in entries):
        return dict(entries)

    roots = {path.split("/", 1)[0] for path, _ in entries}
    if len(roots) != 1:
        return dict(entries)
    if any("/" not in path for path, _ in entries):
        return dict(entries)

    return {path.split("/", 1)[1]: raw for path, raw in entries}
