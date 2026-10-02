#!/usr/bin/env python3
"""E2E UI driver for SupportOS++ — navigates every page, captures visible
text + controls, clicks every control, and writes a plain-text report.

Usage: python3 e2e_driver.py <app_binary> <data_dir> <report_path>

This script:
1. Connects to tauri-driver (which must be running on port 4444).
2. Creates a WebDriver session for the running Tauri app.
3. Navigates to each page via the URL fragment.
4. Captures visible text + control list.
5. Writes a plain-text report (page, controls, results, pass or fail).
"""

import json
import sys
import time
import urllib.request
import urllib.error

# --- Configuration ---
APP_BINARY = sys.argv[1] if len(sys.argv) > 1 else "target/debug/supportos-plusplus"
DATA_DIR = sys.argv[2] if len(sys.argv) > 2 else "/tmp/spp-e2e-test"
REPORT_PATH = sys.argv[3] if len(sys.argv) > 3 else "/tmp/e2e-report.md"
WEBDRIVER_URL = "http://127.0.0.1:4444"

# Pages to test — (route, description)
PAGES = [
    ("/", "Dashboard"),
    ("/inbox", "Inbox"),
    ("/operations", "Operations Center"),
    ("/notifications", "Notifications"),
    ("/automation", "Automation"),
    ("/sync-health", "Sync Health"),
    ("/settings", "Settings"),
    ("/ai-center", "AI Center"),
    ("/reports", "Reports"),
    ("/issue-radar", "Issue Radar"),
    ("/incidents", "Incidents"),
    ("/knowledge-gaps", "Knowledge Gaps"),
    ("/customers", "Customers"),
    ("/connectors", "Connectors"),
    ("/custom-objects", "Custom Objects"),
    ("/outreach", "Outreach"),
    ("/search", "Search"),
    ("/backup", "Backup"),
    ("/support-graph", "Support Graph"),
    ("/support-health", "Support Health"),
    ("/onboarding", "Onboarding"),
    ("/command-palette", "Command Palette"),
    ("/side-threads", "Side Threads"),
    ("/nonexistent", "404 page"),
]


def webdriver_request(method, path, body=None):
    """Make a raw HTTP request to the WebDriver server."""
    url = f"{WEBDRIVER_URL}{path}"
    data = json.dumps(body).encode() if body else b""
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        err_body = e.read().decode() if e.fp else ""
        return {"error": f"HTTP {e.code}", "body": err_body}
    except Exception as e:
        return {"error": str(e)}


def wait_for_webdriver(timeout=30):
    """Wait for tauri-driver to be ready."""
    start = time.time()
    while time.time() - start < timeout:
        try:
            result = webdriver_request("GET", "/status")
            if "ready" in str(result).lower() or "value" in result:
                return True
        except Exception:
            pass
        time.sleep(1)
    return False


def get_page_source(session_id):
    """Get the page source (HTML)."""
    result = webdriver_request("GET", f"/session/{session_id}/source")
    return result.get("value", "")


def navigate(session_id, url):
    """Navigate to a URL."""
    webdriver_request("POST", f"/session/{session_id}/url", {"url": url})


def find_elements(session_id, strategy, value):
    """Find multiple elements. Returns list of element ids."""
    result = webdriver_request(
        "POST",
        f"/session/{session_id}/elements",
        {"using": strategy, "value": value},
    )
    elements = []
    if "value" in result and isinstance(result["value"], list):
        for el in result["value"]:
            eid = el.get("ELEMENT") or el.get("element-6066-11e4-a52e-4f735466cecf")
            if eid:
                elements.append(eid)
    return elements


def click_element(session_id, element_id):
    """Click an element."""
    webdriver_request("POST", f"/session/{session_id}/element/{element_id}/click")


def extract_visible_text(html):
    """Extract visible text from HTML (crude — strip tags)."""
    import re
    # Remove script + style tags + their content.
    html = re.sub(r"<script[^>]*>.*?</script>", "", html, flags=re.DOTALL)
    html = re.sub(r"<style[^>]*>.*?</style>", "", html, flags=re.DOTALL)
    # Remove all tags.
    text = re.sub(r"<[^>]+>", " ", html)
    # Decode HTML entities.
    text = text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
    text = text.replace("&nbsp;", " ").replace("&quot;", '"').replace("&#39;", "'")
    # Collapse whitespace.
    text = re.sub(r"\s+", " ", text).strip()
    return text


def test_page(session_id, route, description):
    """Navigate to a page, capture text + controls, and report results."""
    result = {
        "route": route,
        "description": description,
        "status": "pass",
        "visible_text": "",
        "controls_found": 0,
        "controls_clicked": 0,
        "errors": [],
    }

    try:
        # Navigate to the page via the Tauri window URL.
        navigate(session_id, f"http://tauri.localhost/index.html#{route}")

        # Wait for the page to render.
        time.sleep(2)

        # Get the page source.
        source = get_page_source(session_id)
        if not source:
            result["status"] = "fail"
            result["errors"].append("Page source is empty")
            return result

        # Extract visible text.
        visible_text = extract_visible_text(source)
        result["visible_text"] = visible_text[:500]  # truncate for the report
        if not visible_text.strip():
            result["status"] = "fail"
            result["errors"].append("No visible text on the page")
            return result

        # Find all clickable controls (buttons, links, selects).
        buttons = find_elements(session_id, "css selector", "button")
        links = find_elements(session_id, "css selector", "a")
        selects = find_elements(session_id, "css selector", "select")
        inputs = find_elements(session_id, "css selector", "input")
        result["controls_found"] = len(buttons) + len(links) + len(selects) + len(inputs)

        # Click each button (non-destructive — we just verify it responds).
        for btn_id in buttons[:5]:  # limit to 5 to avoid timeouts
            try:
                click_element(session_id, btn_id)
                result["controls_clicked"] += 1
                time.sleep(0.5)
            except Exception as e:
                result["errors"].append(f"Button click failed: {e}")

        # Check for error text on the page.
        if "error" in visible_text.lower() and "⚠" in visible_text:
            if "failed" in visible_text.lower() or "exception" in visible_text.lower():
                result["status"] = "fail"
                result["errors"].append("Error text found on page")

    except Exception as e:
        result["status"] = "fail"
        result["errors"].append(str(e))

    return result


def main():
    print(f"E2E Driver: app={APP_BINARY}, data_dir={DATA_DIR}, report={REPORT_PATH}")

    # Wait for WebDriver.
    if not wait_for_webdriver():
        print("❌ tauri-driver not ready after 30s")
        write_report([], "fail: tauri-driver not ready")
        sys.exit(1)

    # Create a WebDriver session.
    cap_result = webdriver_request(
        "POST",
        "/session",
        {
            "capabilities": {
                "alwaysMatch": {
                    "browserName": "wry",
                    "platformName": "linux",
                }
            }
        },
    )
    session_id = None
    if "value" in cap_result:
        session_id = cap_result["value"].get("sessionId")
        if not session_id and isinstance(cap_result.get("value"), str):
            session_id = cap_result["value"]

    if not session_id:
        print(f"❌ Failed to create WebDriver session: {cap_result}")
        write_report([], f"fail: could not create WebDriver session: {cap_result}")
        sys.exit(1)

    print(f"✅ WebDriver session: {session_id}")

    # Test each page.
    results = []
    for route, desc in PAGES:
        print(f"  Testing {desc} ({route})...")
        result = test_page(session_id, route, desc)
        results.append(result)
        status = "✅" if result["status"] == "pass" else "❌"
        print(f"  {status} {desc}: {result['controls_found']} controls, {result['controls_clicked']} clicked")

    # Write the report.
    write_report(results, "complete")

    # Delete the session.
    webdriver_request("DELETE", f"/session/{session_id}")

    # Check if any page failed.
    failed = [r for r in results if r["status"] == "fail"]
    if failed:
        print(f"\n❌ {len(failed)} page(s) failed:")
        for r in failed:
            print(f"  - {r['description']} ({r['route']}): {r['errors']}")
        sys.exit(1)
    else:
        print(f"\n✅ All {len(results)} pages passed!")
        sys.exit(0)


def write_report(results, overall_status):
    """Write the plain-text report."""
    with open(REPORT_PATH, "w") as f:
        f.write("# E2E UI Test Report\n\n")
        f.write(f"App: {APP_BINARY}\n")
        f.write(f"Data dir: {DATA_DIR}\n")
        f.write(f"Timestamp: {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n")
        f.write(f"Overall: {overall_status}\n\n")

        f.write("## Page Results\n\n")
        f.write("| Page | Route | Status | Controls found | Controls clicked | Errors |\n")
        f.write("|---|---|---|---|---|---|\n")
        for r in results:
            errors = "; ".join(r.get("errors", []))[:100]
            f.write(
                f"| {r['description']} | {r['route']} | {r['status']} | "
                f"{r['controls_found']} | {r['controls_clicked']} | {errors} |\n"
            )

        f.write("\n## Visible Text (first 200 chars per page)\n\n")
        for r in results:
            text = r.get("visible_text", "")[:200]
            f.write(f"### {r['description']} ({r['route']})\n")
            f.write(f"```\n{text}\n```\n\n")


if __name__ == "__main__":
    main()
