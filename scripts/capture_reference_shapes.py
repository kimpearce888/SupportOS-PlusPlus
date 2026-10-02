#!/usr/bin/env python3
"""Capture reference server's exact response shape for each failing route.

Boots the reference Fastify on :3000, hits each failing route, and
prints the JSON shape (top-level keys + nested keys for 1 level).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

REFERENCE_DIR = Path("/home/z/my-project/supportos")
REFERENCE_PORT = 3000

ROUTES = [
    ("GET", "/api/operations/center", None),
    ("GET", "/api/operations/workload", None),
    ("GET", "/api/settings", None),
    ("GET", "/api/sync/status", None),
    ("GET", "/api/queue", None),
    ("GET", "/api/analytics/dashboard", None),
    ("GET", "/api/analytics/ai", None),
    ("GET", "/api/reports/sla", None),
    ("GET", "/api/ai/status", None),
    ("GET", "/api/ai/jobs", None),
    ("GET", "/api/ai/analytics", None),
    ("GET", "/api/issues/sla-alerts", None),
    ("GET", "/api/automation/rules", None),
    ("GET", "/api/conversations/1/side-threads", None),
    ("GET", "/api/interaction/1", None),
    ("GET", "/api/interaction/profile/1", None),
]


def shape(value, depth=2):
    if depth <= 0:
        return "..."
    if isinstance(value, dict):
        return {k: shape(v, depth - 1) for k, v in value.items()}
    if isinstance(value, list):
        if not value:
            return []
        return [shape(value[0], depth - 1)]
    if isinstance(value, str):
        return f"<str:{value[:40]!r}>"
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return value
    if value is None:
        return None
    return f"<{type(value).__name__}>"


def main():
    env = os.environ.copy()
    env["PORT"] = str(REFERENCE_PORT)
    env["DEMO_MODE"] = "true"
    env["PATH"] = env.get("PATH", "") + ":/usr/bin:/bin"
    proc = subprocess.Popen(
        ["npx", "tsx", "src/server/index.ts"],
        cwd=str(REFERENCE_DIR),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(60):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{REFERENCE_PORT}/health", timeout=0.5) as r:
                if r.status == 200:
                    break
        except Exception:
            time.sleep(0.25)
    else:
        print("FAILED to boot reference server", file=sys.stderr)
        proc.terminate()
        return 1

    try:
        for method, path, body in ROUTES:
            url = f"http://127.0.0.1:{REFERENCE_PORT}{path}"
            data = json.dumps(body).encode() if body else None
            headers = {"Host": f"127.0.0.1:{REFERENCE_PORT}"}
            if body:
                headers["Content-Type"] = "application/json"
            req = urllib.request.Request(url, data=data, headers=headers, method=method)
            try:
                with urllib.request.urlopen(req, timeout=5) as resp:
                    status = resp.status
                    try:
                        payload = json.loads(resp.read().decode())
                    except json.JSONDecodeError:
                        payload = None
            except urllib.error.HTTPError as e:
                status = e.code
                try:
                    payload = json.loads(e.read().decode())
                except Exception:
                    payload = None
            print(f"\n=== {method} {path} → {status} ===")
            print(json.dumps(shape(payload), indent=2, default=str))
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    return 0


if __name__ == "__main__":
    sys.exit(main())
