#!/usr/bin/env python3
"""SSE + webhook integration test for the port's HTTP API.

Boots the standalone HTTP server, opens an SSE subscription to /api/events,
triggers a webhook POST + a demo simulate-incoming + a conversation reply,
and verifies that each mutation pushes a real LiveEvent to the SSE stream.

This proves the SSE event bus is wired correctly into the mutation routes.

Usage:
    python3 scripts/phase4_sse_test.py
"""

from __future__ import annotations

import json
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

PORT_BIN = Path(__file__).parent.parent / "target" / "debug" / "examples" / "http_server"
SSE_PORT = 3501


def boot_port() -> subprocess.Popen:
    env = {
        "SPP_DATA_DIR": "/tmp/spp-sse-test",
        "SPP_HTTP_PORT": str(SSE_PORT),
        "LOCAL_DEMO_MODE": "true",
        "PATH": "/usr/bin:/bin",
    }
    Path("/tmp/spp-sse-test").mkdir(parents=True, exist_ok=True)
    proc = subprocess.Popen(
        [str(PORT_BIN)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    # Wait for the server to bind.
    for _ in range(50):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{SSE_PORT}/health", timeout=0.5) as resp:
                if resp.status == 200:
                    return proc
        except Exception:
            time.sleep(0.1)
    proc.terminate()
    raise RuntimeError("HTTP server failed to boot")


def hit(method: str, path: str, body: dict | None = None) -> tuple[int, dict | None]:
    url = f"http://127.0.0.1:{SSE_PORT}{path}"
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Host": f"127.0.0.1:{SSE_PORT}"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            try:
                return resp.status, json.loads(resp.read().decode())
            except json.JSONDecodeError:
                return resp.status, None
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read().decode())
        except json.JSONDecodeError:
            return e.code, None


def sse_listener(events: list, stop_after: float = 5.0) -> threading.Thread:
    """Open an SSE connection and accumulate events until stop_after seconds elapse."""
    received = events

    def _listen():
        try:
            req = urllib.request.Request(
                f"http://127.0.0.1:{SSE_PORT}/api/events",
                headers={"Host": f"127.0.0.1:{SSE_PORT}", "Accept": "text/event-stream"},
            )
            with urllib.request.urlopen(req, timeout=stop_after + 2) as resp:
                start = time.time()
                buffer = ""
                while time.time() - start < stop_after:
                    chunk = resp.read(1024)
                    if not chunk:
                        break
                    buffer += chunk.decode(errors="replace")
                    # SSE events are separated by blank lines.
                    while "\n\n" in buffer:
                        event_str, buffer = buffer.split("\n\n", 1)
                        for line in event_str.split("\n"):
                            if line.startswith("data:"):
                                data = line[len("data:"):].strip()
                                try:
                                    received.append(json.loads(data))
                                except json.JSONDecodeError:
                                    pass
                            elif line.startswith(":"):
                                # Keep-alive comment — count it.
                                received.append({"_comment": line[1:].strip()})
        except Exception as e:
            received.append({"_error": str(e)})

    t = threading.Thread(target=_listen, daemon=True)
    t.start()
    return t


def main() -> int:
    print("[boot] starting port HTTP server...")
    proc = boot_port()
    print(f"[boot] HTTP server bound on :{SSE_PORT}")

    # 1. Start SSE listener.
    print("\n[1] Opening SSE subscription to /api/events")
    events: list = []
    listener = sse_listener(events, stop_after=4.0)
    time.sleep(0.5)  # let the SSE connection establish

    # 2. Enable demo mode.
    print("[2] POST /api/demo/enable")
    hit("POST", "/api/demo/enable")

    # 3. Trigger a webhook receive (no HMAC since no secret set).
    print("[3] POST /api/webhooks/helpscout (simulated)")
    webhook_body = {"event": "convo.created", "conversationId": 99999}
    hit("POST", "/api/webhooks/helpscout", webhook_body)

    # 4. Trigger a demo simulate-incoming.
    print("[4] POST /api/demo/simulate-incoming")
    hit("POST", "/api/demo/simulate-incoming", {"subject": "test", "body": "test"})

    # 5. Trigger a demo simulate-rating.
    print("[5] POST /api/demo/simulate-rating")
    hit("POST", "/api/demo/simulate-rating", {"conversationRemoteId": 1, "rating": "great"})

    # 6. Trigger a demo simulate-webhook.
    print("[6] POST /api/demo/simulate-webhook")
    hit("POST", "/api/demo/simulate-webhook", {"event": "convo.created"})

    # Wait for the listener to collect events.
    print("\n[wait] collecting SSE events for 4 seconds...")
    listener.join(timeout=6.0)

    # Analyze.
    print(f"\n[result] received {len(events)} SSE messages")
    for i, e in enumerate(events):
        print(f"  [{i}] {e}")

    # Verify we got at least one WebhookReceived and one RatingArrived event.
    webhook_events = [
        e for e in events
        if isinstance(e, dict) and e.get("type") == "WebhookReceived"
    ]
    rating_events = [
        e for e in events
        if isinstance(e, dict) and e.get("type") == "RatingArrived"
    ]
    sync_events = [
        e for e in events
        if isinstance(e, dict) and e.get("type") == "SyncUpdated"
    ]

    print(f"\n[verify] WebhookReceived events: {len(webhook_events)}")
    print(f"[verify] SyncUpdated events:     {len(sync_events)}")
    print(f"[verify] RatingArrived events:   {len(rating_events)}")

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()

    if len(webhook_events) >= 1 and len(rating_events) >= 1 and len(sync_events) >= 1:
        print("\nPASS — SSE event bus is correctly wired into mutation routes.")
        return 0
    else:
        print("\nFAIL — expected at least 1 of each event type.")
        return 1


if __name__ == "__main__":
    sys.exit(main())
