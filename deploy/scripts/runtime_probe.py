#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Run a small Python probe inside a Docker-managed QwenPaw runtime.

Only stdlib + the host's ``docker`` CLI. The probe runs *inside* the runtime
container so it can use the same ``127.0.0.1:${QWENPAW_PORT}`` + internal token
path the plugin uses; that is the only reliable way to test Docker Hub runtimes
whose published port is bound to a random loopback port on the host.
"""

from __future__ import annotations

import json
import subprocess
from typing import Optional

MARKER = "__RUNTIME_PROBE__"


def _exec(container: str, args: list[str], timeout: float) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["docker", "exec", container, *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
    )


def find_python(container: str, timeout: float = 30.0) -> str:
    """Return ``python3``/``python`` available inside the container."""
    problems: list[str] = []
    for executable in ("python3", "python"):
        try:
            completed = _exec(container, [executable, "-c", "print('runtime-probe-ok')"], timeout)
        except FileNotFoundError as exc:
            raise RuntimeError("找不到 docker 命令") from exc
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError(f"docker exec 超时（{timeout}s）") from exc
        except OSError as exc:
            raise RuntimeError(f"docker exec 失败：{exc}") from exc
        if completed.returncode == 0 and "runtime-probe-ok" in (completed.stdout or ""):
            return executable
        problems.append(((completed.stderr or "") + (completed.stdout or "")).strip())
    raise RuntimeError("容器里没有可用的 python3/python：" + "；".join(problems)[:400])


def run_python(
    container: str,
    code: str,
    arg: Optional[str] = None,
    timeout: float = 360.0,
) -> subprocess.CompletedProcess:
    """Run ``python -c code [arg]`` inside the container."""
    executable = find_python(container)
    args = [executable, "-c", code]
    if arg is not None:
        args.append(arg)
    try:
        return _exec(container, args, timeout)
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(f"容器内探针超时（{timeout}s）") from exc


def parse_result(output: str, marker: str = MARKER) -> Optional[dict]:
    """Extract the last JSON line prefixed by *marker*."""
    for line in reversed((output or "").splitlines()):
        if line.startswith(marker):
            try:
                payload = json.loads(line[len(marker) :])
            except json.JSONDecodeError:
                return None
            return payload if isinstance(payload, dict) else None
    return None
