#!/usr/bin/env python3
"""Raw socket SSE test — bypass urllib buffering.

Opens a raw TCP socket to the SSE endpoint, sends a GET request,
and reads the response bytes as they arrive.
"""

from __future__ import annotations

import json
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

PORT_BIN = Path(__file__).parent.parent / "target" / "debug" / "examples" / "http_server"
SSE_PORT = 3502


def boot_port() -> subprocess.Popen:
    env = {
        "SPP_DATA_DIR": "/tmp/spp-sse-raw-test",
        "SPP_HTTP_PORT": str(SSE_PORT),
        "LOCAL_DEMO_MODE": "true",
        "PATH": "/usr/bin:/bin",
    }
    Path("/tmp/spp-sse-raw-test").mkdir(parents=True, exist_ok=True)
    proc = subprocess.Popen(
        [str(PORT_BIN)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    for _ in range(50):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{SSE_PORT}/health", timeout=0.5) as resp:
                if resp.status == 200:
                    return proc
        except Exception:
            time.sleep(0.1)
    proc.terminate()
    raise RuntimeError("HTTP server failed to boot")


def raw_sse(port: int, events: list, stop_after: float = 5.0) -> threading.Thread:
    def _listen():
        try:
            sock = socket.create_connection(("127.0.0.1", port), timeout=stop_after + 2)
            req = (
                f"GET /api/events HTTP/1.1\r\n"
                f"Host: 127.0.0.1:{port}\r\n"
                f"Accept: text/event-stream\r\n"
                f"Connection: keep-alive\r\n"
                f"\r\n"
            )
            sock.sendall(req.encode())
            sock.settimeout(stop_after)
            buffer = ""
            start = time.time()
            while time.time() - start < stop_after:
                try:
                    chunk = sock.recv(4096)
                except socket.timeout:
                    break
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
                                events.append(json.loads(data))
                            except json.JSONDecodeError:
                                events.append({"_raw_data": data})
                        elif line.startswith(":"):
                            events.append({"_comment": line[1:].strip()})
            sock.close()
        except Exception as e:
            events.append({"_error": str(e)})

    t = threading.Thread(target=_listen, daemon=True)
    t.start()
    return t


def hit(method: str, path: str, body: dict | None = None) -> None:
    url = f"http://127.0.0.1:{SSE_PORT}{path}"
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Host": f"127.0.0.1:{SSE_PORT}"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            print(f"  -> {method} {path} = {resp.status}")
    except urllib.error.HTTPError as e:
        print(f"  -> {method} {path} = HTTP {e.code}")
    except Exception as e:
        print(f"  -> {method} {path} = ERROR: {e}")


def main() -> int:
    print("[boot] starting port HTTP server...")
    proc = boot_port()
    print(f"[boot] HTTP server bound on :{SSE_PORT}")

    print("\n[1] Opening raw socket SSE subscription to /api/events")
    events: list = []
    listener = raw_sse(SSE_PORT, events, stop_after=5.0)
    time.sleep(1.0)  # let the SSE connection establish

    print("\n[2] POST /api/demo/enable")
    hit("POST", "/api/demo/enable")
    time.sleep(0.2)

    print("[3] POST /api/webhooks/helpscout (simulated)")
    hit("POST", "/api/webhooks/helpscout", {"event": "convo.created", "conversationId": 99999})
    time.sleep(0.2)

    print("[4] POST /api/demo/simulate-incoming")
    hit("POST", "/api/demo/simulate-incoming", {"subject": "test"})
    time.sleep(0.2)

    print("[5] POST /api/demo/simulate-rating")
    hit("POST", "/api/demo/simulate-rating", {"conversationRemoteId": 1, "rating": "great"})
    time.sleep(0.2)

    print("[6] POST /api/demo/simulate-webhook")
    hit("POST", "/api/demo/simulate-webhook", {"event": "convo.created"})

    print("\n[wait] collecting SSE events for 5 seconds...")
    listener.join(timeout=7.0)

    print(f"\n[result] received {len(events)} SSE messages")
    for i, e in enumerate(events):
        print(f"  [{i}] {e}")

    webhook_events = [e for e in events if isinstance(e, dict) and e.get("type") == "WebhookReceived"]
    sync_events = [e for e in events if isinstance(e, dict) and e.get("type") == "SyncUpdated"]
    rating_events = [e for e in events if isinstance(e, dict) and e.get("type") == "RatingArrived"]

    print(f"\n[verify] WebhookReceived events: {len(webhook_events)}")
    print(f"[verify] SyncUpdated events:     {len(sync_events)}")
    print(f"[verify] RatingArrived events:   {len(rating_events)}")

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()

    if len(webhook_events) >= 1 and len(rating_events) >= 1 and len(sync_events) >= 1:
        print("\nPASS - SSE event bus is correctly wired into mutation routes.")
        return 0
    print("\nFAIL - expected at least 1 of each event type.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
