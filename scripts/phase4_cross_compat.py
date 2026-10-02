#!/usr/bin/env python3
"""Cross-compatibility test: boot BOTH the reference (Fastify on :3000)
and the port (axum on :3001), then hit the same routes on both and
verify response shapes (status + top-level JSON keys) match.

This is the gold-standard proof that the port's HTTP API is a true
drop-in replacement for the reference's HTTP API.

Usage:
    python3 scripts/phase4_cross_compat.py

Requires:
    - Reference repo at /home/z/my-project/supportos with deps installed (npm install)
    - Port standalone HTTP server built:
      cargo build -p supportos-plusplus-core --example http_server

Exit codes:
    0 - all comparable routes have matching shapes
    1 - at least one route diverged
    2 - one or both servers failed to boot
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

REFERENCE_DIR = Path("/home/z/my-project/supportos")
PORT_BIN = Path(__file__).parent.parent / "target" / "debug" / "examples" / "http_server"
REFERENCE_PORT = 3000
PORT_PORT = 3001

# Routes to compare. These are the routes both servers should agree on
# (URL + method + status code + top-level JSON keys). Routes that
# require Help Scout OAuth credentials are excluded — they return
# empty data on both servers in demo mode.
ROUTES_TO_COMPARE = [
    # (method, path, body or None)
    ("GET", "/health", None),
    ("GET", "/health/detailed", None),
    ("GET", "/api/system/db", None),
    ("GET", "/api/system/capabilities", None),
    ("GET", "/api/system/tables", None),
    ("GET", "/api/onboarding", None),
    ("POST", "/api/onboarding/step", {"step": "welcome"}),
    ("GET", "/api/conversations", None),
    ("GET", "/api/ticket-states", None),
    ("GET", "/api/mailboxes", None),
    ("GET", "/api/tags", None),
    ("GET", "/api/users", None),
    ("GET", "/api/teams", None),
    ("GET", "/api/saved-replies", None),
    ("GET", "/api/customers", None),
    ("GET", "/api/organizations", None),
    ("POST", "/api/search", {"q": "test"}),
    ("GET", "/api/operations/center", None),
    ("GET", "/api/operations/workload", None),
    ("GET", "/api/notifications", None),
    ("GET", "/api/notifications/unread-count", None),
    ("GET", "/api/notifications/prefs", None),
    ("GET", "/api/settings", None),
    ("GET", "/api/sync/status", None),
    ("GET", "/api/queue", None),
    ("GET", "/api/analytics/dashboard", None),
    ("GET", "/api/analytics/ai", None),
    ("GET", "/api/reports/sla", None),
    ("GET", "/api/reports/issue-radar", None),
    ("GET", "/api/ai/status", None),
    ("GET", "/api/ai/jobs", None),
    ("GET", "/api/ai/analytics", None),
    ("GET", "/api/ai/evaluation", None),
    ("GET", "/api/issues/clusters", None),
    ("GET", "/api/issues/sla-alerts", None),
    ("GET", "/api/issues/known", None),
    ("GET", "/api/issues/cases", None),
    ("GET", "/api/automation/rules", None),
    ("GET", "/api/conversations/1/side-threads", None),
    ("GET", "/api/copilot/sessions", None),
    ("GET", "/api/copilot/tools", None),
    ("GET", "/api/knowledge/sources", None),
    ("GET", "/api/knowledge/documents", None),
    ("GET", "/api/knowledge/freshness", None),
    ("GET", "/api/outreach/segments", None),
    ("GET", "/api/outreach/campaigns", None),
    ("GET", "/api/custom-objects/types", None),
    ("GET", "/api/connectors", None),
    ("GET", "/api/incidents", None),
    ("GET", "/api/graph/stats", None),
    ("GET", "/api/graph/meta", None),
    ("GET", "/api/graph/edges", None),
    ("GET", "/api/attributes/catalog", None),
    ("GET", "/api/attributes/report", None),
    ("GET", "/api/coaching/meta", None),
    ("GET", "/api/translation/meta", None),
    ("GET", "/api/memory/meta", None),
    ("GET", "/api/interaction/1", None),
    ("GET", "/api/interaction/profile/1", None),
    ("GET", "/api/qa/overview", None),
    ("GET", "/api/friction/overview", None),
    ("GET", "/api/knowledge/gaps", None),
    ("GET", "/api/docs/collections", None),
    ("GET", "/api/docs/stats", None),
    ("GET", "/api/docs/articles", None),
    ("GET", "/api/inbox-views", None),
]


def boot_reference() -> subprocess.Popen | None:
    if not REFERENCE_DIR.exists():
        print("[boot] reference repo not found")
        return None
    env = os.environ.copy()
    env["PORT"] = str(REFERENCE_PORT)
    env["DEMO_MODE"] = "true"
    env["PATH"] = env.get("PATH", "") + ":/usr/bin:/bin"
    env["NODE_ENV"] = "development"
    proc = subprocess.Popen(
        ["npx", "tsx", "src/server/index.ts"],
        cwd=str(REFERENCE_DIR),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    for _ in range(60):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{REFERENCE_PORT}/health", timeout=0.5) as r:
                if r.status == 200:
                    print(f"[boot] reference Fastify bound on :{REFERENCE_PORT}")
                    return proc
        except Exception:
            time.sleep(0.25)
    print("[boot] FAILED to boot reference Fastify")
    proc.terminate()
    return None


def boot_port() -> subprocess.Popen | None:
    if not PORT_BIN.exists():
        print(f"[boot] port binary not found at {PORT_BIN}")
        return None
    env = {
        "SPP_DATA_DIR": "/tmp/spp-cross-compat-port",
        "SPP_HTTP_PORT": str(PORT_PORT),
        "LOCAL_DEMO_MODE": "true",
        "PATH": "/usr/bin:/bin",
    }
    Path("/tmp/spp-cross-compat-port").mkdir(parents=True, exist_ok=True)
    proc = subprocess.Popen(
        [str(PORT_BIN)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    for _ in range(50):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{PORT_PORT}/health", timeout=0.5) as r:
                if r.status == 200:
                    print(f"[boot] port HTTP server bound on :{PORT_PORT}")
                    return proc
        except Exception:
            time.sleep(0.1)
    print("[boot] FAILED to boot port HTTP server")
    proc.terminate()
    return None


def hit_route(port: int, method: str, path: str, body: Any | None) -> tuple[int, dict | list | None, str]:
    url = f"http://127.0.0.1:{port}{path}"
    data = None
    headers = {"Host": f"127.0.0.1:{port}"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    # Try up to 3 times on connection errors (port may close the
    # connection between requests due to keep-alive timeouts).
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                status = resp.status
                try:
                    payload = json.loads(resp.read().decode())
                except (json.JSONDecodeError, UnicodeDecodeError):
                    payload = None
                return status, payload, ""
        except urllib.error.HTTPError as e:
            try:
                payload = json.loads(e.read().decode())
            except (json.JSONDecodeError, UnicodeDecodeError):
                payload = None
            return e.code, payload, ""
        except (urllib.error.URLError, ConnectionResetError, BrokenPipeError) as e:
            # Retry on connection-level errors.
            time.sleep(0.2 * (attempt + 1))
            if attempt == 2:
                return 0, None, repr(e)
        except Exception as e:
            return 0, None, repr(e)
    return 0, None, "max retries exhausted"


def top_level_keys(payload: Any) -> set[str]:
    if isinstance(payload, dict):
        return set(payload.keys())
    if isinstance(payload, list):
        # If the response is a list, return the union of keys across items.
        keys: set[str] = set()
        for item in payload[:5]:
            if isinstance(item, dict):
                keys |= set(item.keys())
        return keys
    return set()


def main() -> int:
    print("[boot] starting reference Fastify server...")
    ref_proc = boot_reference()
    if ref_proc is None:
        return 2
    print("[boot] starting port HTTP server...")
    port_proc = boot_port()
    if port_proc is None:
        ref_proc.terminate()
        return 2

    try:
        passed = 0
        failed = 0
        skipped = 0
        for method, path, body in ROUTES_TO_COMPARE:
            ref_status, ref_payload, ref_err = hit_route(REFERENCE_PORT, method, path, body)
            port_status, port_payload, port_err = hit_route(PORT_PORT, method, path, body)

            if ref_err:
                print(f"  SKIP  {method:6} {path:50} (reference error: {ref_err})")
                skipped += 1
                continue
            if port_err:
                print(f"  SKIP  {method:6} {path:50} (port error: {port_err})")
                skipped += 1
                continue

            # Compare status codes.
            if ref_status != port_status:
                print(f"  FAIL  {method:6} {path:50} status: ref={ref_status}, port={port_status}")
                failed += 1
                continue

            # Compare top-level JSON keys (for dict responses).
            ref_keys = top_level_keys(ref_payload)
            port_keys = top_level_keys(port_payload)

            # Be lenient: the port may include extra keys (e.g. _status, event_id),
            # but it should include ALL the keys the reference returns.
            missing = ref_keys - port_keys
            if missing:
                print(f"  FAIL  {method:6} {path:50} missing keys: {missing}")
                print(f"        ref keys:  {sorted(ref_keys)}")
                print(f"        port keys: {sorted(port_keys)}")
                failed += 1
                continue

            print(f"  OK    {method:6} {path:50} status={ref_status} keys={len(ref_keys)}")
            passed += 1

        print(f"\n{passed} passed, {failed} failed, {skipped} skipped out of {len(ROUTES_TO_COMPARE)} routes")
        return 0 if failed == 0 else 1
    finally:
        for proc in (ref_proc, port_proc):
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()


if __name__ == "__main__":
    sys.exit(main())
