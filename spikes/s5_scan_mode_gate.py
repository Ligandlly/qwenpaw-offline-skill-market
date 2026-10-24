# -*- coding: utf-8 -*-
"""Deployment preflight: is the host's skill scanner actually a gate?

Spike result (2026-10-24, qwenpaw 2.2.2, fresh install on Windows):
  - default scanner mode is ``warn``  -> CRITICAL finding is logged and the skill
    is INSTALLED AND ENABLED anyway (verified: exec(input()) landed on disk)
  - with ``QWENPAW_SKILL_SCAN_MODE=block`` -> install fails with
    ``security_scan_failed`` and nothing is left on disk

So an offline market that relies on the host scanner MUST force ``block``.
This script is the gate for that requirement; it is also what our provider's
``available()`` should call so the market refuses to serve instead of
silently distributing unscanned code.

Usage:  python s5_scan_mode_gate.py            # exit 0 = safe to serve
        python s5_scan_mode_gate.py --json
"""

from __future__ import annotations

import json
import os
import sys

VALID = {"block", "warn", "off"}
REQUIRED = "block"


def resolve_mode() -> tuple[str, str, str]:
    """Return (mode, source, detail) mirroring qwenpaw's own precedence."""
    env = os.environ.get("QWENPAW_SKILL_SCAN_MODE")
    if env and env.lower().strip() in VALID:
        return env.lower().strip(), "env", "QWENPAW_SKILL_SCAN_MODE"

    detail = "no config found"
    try:
        from qwenpaw.config.utils import get_config_path  # type: ignore

        cfg_path = get_config_path()
    except Exception:
        cfg_path = None

    if cfg_path is None:
        home = os.environ.get("QWENPAW_WORKING_DIR") or os.path.expanduser("~/.qwenpaw")
        cfg_path = os.path.join(home, "config.json")

    try:
        with open(cfg_path, encoding="utf-8") as fh:
            cfg = json.load(fh)
        mode = str(cfg.get("security", {}).get("skill_scanner", {}).get("mode", "block"))
        detail = str(cfg_path)
        if mode.lower() in VALID:
            return mode.lower(), "config", detail
        return "block", "default", f"{detail} (invalid mode {mode!r})"
    except FileNotFoundError:
        return "block", "default", f"{cfg_path} missing"
    except Exception as exc:  # pragma: no cover
        return "warn", "unknown", f"could not read config: {exc}"


def main() -> int:
    mode, source, detail = resolve_mode()
    ok = mode == REQUIRED
    reason = (
        "scanner is a real gate"
        if ok
        else "scanner does NOT block CRITICAL findings; this market must not serve"
    )
    report = {
        "ok": ok,
        "mode": mode,
        "required": REQUIRED,
        "source": source,
        "detail": detail,
        "reason": reason,
        "fix": None if ok else "set QWENPAW_SKILL_SCAN_MODE=block for the runtime",
    }
    if "--json" in sys.argv:
        print(json.dumps(report, indent=2, ensure_ascii=False))
    else:
        print(f"scan mode: {mode}  (from {source}: {detail})")
        print(f"{'PASS' if ok else 'FAIL'}: {reason}")
        if not ok:
            print(f"fix: {report['fix']}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
