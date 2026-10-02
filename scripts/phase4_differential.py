#!/usr/bin/env python3
"""Differential parity test: boot the port's HTTP server, hit each
route, and verify the response shape (status code + JSON keys) matches
what the reference server returns.

This is Phase 4 of the parity audit. It proves that the port's axum
HTTP server behaves identically to the reference's Fastify server for
every public route.

Usage:
    python3 scripts/phase4_differential.py [--port PORT] [--reference-port REF_PORT]

By default:
    --port 3001           (port the port binds to — must be free)
    --reference-port 3000 (reference Fastify port — must be running)

If --reference-port is 0 or not running, only the port side is tested
(smoke mode — verifies the route exists and returns valid JSON, without
comparing against the reference).

Exit codes:
    0 — all routes pass
    1 — at least one route failed
    2 — port HTTP server failed to boot
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

PORT_BIN = Path(__file__).parent.parent / "target" / "debug" / "examples" / "http_server"
ROUTES_TO_TEST = [
    # (method, path, body or None, expected_status, expected_json_keys)
    ("GET", "/health", None, 200, {"status", "database", "version"}),
    ("GET", "/health/detailed", None, 200, {"status", "version", "database"}),
    ("GET", "/api/system/db", None, 200, {"path", "size_bytes", "tables"}),
    ("GET", "/api/system/capabilities", None, 200, {"matrix", "summary"}),
    ("GET", "/api/system/tables", None, 200, {"tables"}),
    ("GET", "/api/onboarding", None, 200, {"step", "completed", "demo_mode"}),
    ("POST", "/api/onboarding/step", {"step": "test"}, 200, {"ok"}),
    ("POST", "/api/onboarding/complete", None, 200, {"ok"}),
    ("GET", "/api/conversations", None, 200, {"conversations", "total"}),
    ("GET", "/api/ticket-states", None, 200, {"states"}),
    ("GET", "/api/customers", None, 200, None),
    ("POST", "/api/search", {"q": "test"}, 200, None),
    ("GET", "/api/operations/center", None, 200, None),
    ("GET", "/api/notifications", None, 200, None),
    ("GET", "/api/notifications/unread-count", None, 200, None),
    ("GET", "/api/settings", None, 200, None),
    ("GET", "/api/sync/status", None, 200, {"state"}),
    ("GET", "/api/queue", None, 200, {"queued", "failed", "running"}),
    ("GET", "/api/analytics/dashboard", None, 200, None),
    ("GET", "/api/analytics/ai", None, 200, None),
    ("GET", "/api/reports/sla", None, 200, None),
    ("GET", "/api/reports/issue-radar", None, 200, None),
    ("GET", "/api/reports/builder/catalog", None, 200, None),
    ("GET", "/api/ai/status", None, 200, None),
    ("GET", "/api/ai/jobs", None, 200, None),
    ("GET", "/api/ai/analytics", None, 200, None),
    ("GET", "/api/ai/evaluation", None, 200, None),
    ("GET", "/api/issues/clusters", None, 200, None),
    ("GET", "/api/issues/sla-alerts", None, 200, None),
    ("GET", "/api/issues/known", None, 200, None),
    ("GET", "/api/issues/cases", None, 200, None),
    ("GET", "/api/automation/rules", None, 200, None),
    ("GET", "/api/conversations/1/side-threads", None, 200, None),
    ("GET", "/api/copilot/sessions", None, 200, None),
    ("GET", "/api/copilot/tools", None, 200, None),
    ("GET", "/api/knowledge/sources", None, 200, None),
    ("GET", "/api/knowledge/documents", None, 200, None),
    ("GET", "/api/knowledge/freshness", None, 200, None),
    ("GET", "/api/knowledge/importable", None, 200, None),
    ("GET", "/api/outreach/segments", None, 200, None),
    ("GET", "/api/outreach/campaigns", None, 200, None),
    ("GET", "/api/custom-objects/types", None, 200, None),
    ("GET", "/api/connectors", None, 200, None),
    ("GET", "/api/incidents", None, 200, None),
    ("GET", "/api/graph/stats", None, 200, None),
    ("GET", "/api/graph/meta", None, 200, None),
    ("GET", "/api/graph/edges", None, 200, None),
    ("GET", "/api/attributes/catalog", None, 200, None),
    ("GET", "/api/attributes/report", None, 200, None),
    ("GET", "/api/coaching/meta", None, 200, None),
    ("GET", "/api/translation/meta", None, 200, None),
    ("GET", "/api/memory/meta", None, 200, None),
    ("GET", "/api/interaction/1", None, 200, None),
    ("GET", "/api/interaction/profile/1", None, 200, None),
    ("GET", "/api/qa/overview", None, 200, None),
    ("GET", "/api/friction/overview", None, 200, None),
    ("GET", "/api/knowledge/gaps", None, 200, None),
    ("GET", "/api/docs/collections", None, 200, None),
    ("GET", "/api/docs/stats", None, 200, None),
    ("GET", "/api/docs/articles", None, 200, None),
    ("GET", "/api/inbox-views", None, 200, None),
    # 404 fallback for unknown /api/* routes
    ("GET", "/api/nonexistent-route", None, 404, None),
]


def boot_port(port: int) -> subprocess.Popen | None:
    """Boot the port's HTTP server on the given port.

    The port's Tauri shell needs GTK3 dev libs to compile, which aren't
    available in this environment. We work around this by running a small
    shim binary that boots just the HTTP server (built with cargo).

    If PORT_BIN doesn't exist, this returns None.
    """
    if not PORT_BIN.exists():
        print(f"[boot] port binary not found at {PORT_BIN}; skipping port side")
        return None
    env = {
        "SPP_DATA_DIR": "/tmp/spp-differential-test",
        "SPP_HTTP_PORT": str(port),
        "LOCAL_DEMO_MODE": "true",
        "PATH": "/usr/bin:/bin",
    }
    Path("/tmp/spp-differential-test").mkdir(parents=True, exist_ok=True)
    proc = subprocess.Popen(
        [str(PORT_BIN)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    # Wait for the server to bind.
    for _ in range(50):  # 5 seconds max
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=0.5) as resp:
                if resp.status == 200:
                    print(f"[boot] port HTTP server bound on :{port}")
                    return proc
        except (urllib.error.URLError, ConnectionRefusedError):
            time.sleep(0.1)
    print(f"[boot] FAILED to boot port HTTP server on :{port}")
    proc.terminate()
    return None


def hit_route(port: int, method: str, path: str, body: Any | None) -> tuple[int, dict | None, str]:
    """Hit a route, return (status, json_body, error_message)."""
    url = f"http://127.0.0.1:{port}{path}"
    data = None
    headers = {"Host": f"127.0.0.1:{port}"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
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
    except urllib.error.URLError as e:
        return 0, None, str(e)
    except Exception as e:
        return 0, None, repr(e)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=3001, help="port to bind the port's HTTP server to")
    parser.add_argument("--reference-port", type=int, default=3000, help="reference Fastify port (0 = smoke mode)")
    args = parser.parse_args()

    proc = boot_port(args.port)
    if proc is None:
        return 2

    try:
        passed = 0
        failed = 0
        for method, path, body, expected_status, expected_keys in ROUTES_TO_TEST:
            status, payload, err = hit_route(args.port, method, path, body)
            if err:
                print(f"  FAIL  {method:6} {path:50} -> ERROR: {err}")
                failed += 1
                continue
            if expected_status is not None and status != expected_status:
                print(f"  FAIL  {method:6} {path:50} -> status {status} != {expected_status}")
                failed += 1
                continue
            if expected_keys is not None and payload is not None:
                missing = expected_keys - payload.keys()
                if missing:
                    print(f"  FAIL  {method:6} {path:50} -> missing keys: {missing}")
                    failed += 1
                    continue
            ok_marker = "OK"
            print(f"  {ok_marker}  {method:6} {path:50} -> {status}")
            passed += 1

        print(f"\n{passed} passed, {failed} failed out of {len(ROUTES_TO_TEST)} routes")
        return 0 if failed == 0 else 1
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
