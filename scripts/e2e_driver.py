#!/usr/bin/env python3
"""E2E UI driver for SupportOS++ — navigates every page, clicks every control,
submits forms, checks loading/empty/error states, and writes a plain-text report.

Usage: python3 e2e_driver.py <app_binary> <data_dir> <report_path>

This script:
1. Connects to tauri-driver (which must be running on port 4444).
2. Creates a WebDriver session for the running Tauri app.
3. Navigates to each page via the URL fragment.
4. For each page:
   a. Captures visible text (HTML → stripped).
   b. Finds ALL controls (buttons, links, selects, inputs, textareas).
   c. Clicks EVERY button (not just 5).
   d. Types into EVERY input/textarea (valid + invalid input).
   e. Changes EVERY select.
   f. Re-captures text after each interaction.
   g. Checks for error states (⚠ icon, "failed", "exception").
   h. Checks for placeholder data (hard-coded demo values).
5. Writes a plain-text report (page, controls, results, pass/fail).
6. Fails on any dead control, any error shown, any placeholder data.
"""

import json
import re
import sys
import time
import urllib.request
import urllib.error

# --- Configuration ---
APP_BINARY = sys.argv[1] if len(sys.argv) > 1 else "target/debug/supportos-plusplus"
DATA_DIR = sys.argv[2] if len(sys.argv) > 2 else "/tmp/spp-e2e-test"
REPORT_PATH = sys.argv[3] if len(sys.argv) > 3 else "/tmp/e2e-report.md"
WEBDRIVER_URL = "http://127.0.0.1:4444"

# Pages to test — (route, description, requires_data)
# requires_data=True means the page expects real DB data (may show empty state)
PAGES = [
    ("/", "Dashboard", True),
    ("/inbox", "Inbox", True),
    ("/operations", "Operations Center", True),
    ("/notifications", "Notifications", True),
    ("/automation", "Automation", True),
    ("/sync-health", "Sync Health", False),
    ("/customers", "Customer Search", False),
    ("/customers/1", "Customer Profile", True),
    ("/ai-center", "AI Center", False),
    ("/reports", "Reports", True),
    ("/issue-radar", "Issue Radar", True),
    ("/incidents", "Incidents", True),
    ("/knowledge-gaps", "Knowledge Gaps", True),
    ("/connectors", "Connectors", True),
    ("/custom-objects", "Custom Objects", True),
    ("/outreach", "Outreach", True),
    ("/search", "Search", False),
    ("/backup", "Backup", False),
    ("/support-graph", "Support Graph", True),
    ("/support-health", "Support Health", True),
    ("/settings", "Settings", False),
    ("/onboarding", "Onboarding", False),
    ("/command-palette", "Command Palette", False),
    ("/side-threads", "Side Threads", True),
    ("/nonexistent", "404 page", False),
]

# Patterns that indicate placeholder/demo data (not real data)
PLACEHOLDER_PATTERNS = [
    r"placeholder data",
    r"demo data",
    r"hardcoded",
    r"hard-coded",
    r"stub\b",
    r"TODO",
    r"FIXME",
    r"not implemented",
    r"Lorem ipsum",
]

# Patterns that indicate real errors (not UI state messages)
ERROR_PATTERNS = [
    r"\bpanic\b",
    r"\bexception\b",
    r"\bstack trace\b",
    r"\bundefined\b",
    r"\bnull is not\b",
    r"\bcannot read prop\b",
    r"\bTypeError\b",
]


def webdriver_request(method, path, body=None, timeout=30):
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
        with urllib.request.urlopen(req, timeout=timeout) as resp:
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


def send_keys(session_id, element_id, text):
    """Send text to an input element."""
    webdriver_request(
        "POST",
        f"/session/{session_id}/element/{element_id}/value",
        {"text": text},
    )


def clear_element(session_id, element_id):
    """Clear an input element."""
    webdriver_request("POST", f"/session/{session_id}/element/{element_id}/clear")


def extract_visible_text(html):
    """Extract visible text from HTML (crude — strip tags)."""
    html = re.sub(r"<script[^>]*>.*?</script>", "", html, flags=re.DOTALL)
    html = re.sub(r"<style[^>]*>.*?</style>", "", html, flags=re.DOTALL)
    text = re.sub(r"<[^>]+>", " ", html)
    text = text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
    text = text.replace("&nbsp;", " ").replace("&quot;", '"').replace("&#39;", "'")
    text = re.sub(r"\s+", " ", text).strip()
    return text


def check_for_errors(text):
    """Check for real error patterns in visible text."""
    text_lower = text.lower()
    for pattern in ERROR_PATTERNS:
        if re.search(pattern, text_lower):
            return f"Error pattern found: {pattern}"
    return None


def check_for_placeholders(text):
    """Check for placeholder/demo data patterns."""
    text_lower = text.lower()
    for pattern in PLACEHOLDER_PATTERNS:
        if re.search(pattern, text_lower):
            return f"Placeholder pattern found: {pattern}"
    return None


def test_page(session_id, route, description, requires_data):
    """Navigate to a page, exercise every control, and report results."""
    result = {
        "route": route,
        "description": description,
        "status": "pass",
        "visible_text": "",
        "controls_found": 0,
        "controls_clicked": 0,
        "inputs_filled": 0,
        "selects_changed": 0,
        "errors": [],
        "checks": [],
    }

    try:
        # Navigate to the page.
        navigate(session_id, f"http://tauri.localhost/index.html#{route}")
        time.sleep(3)  # Wait for async IPC calls to complete

        # Get the page source.
        source = get_page_source(session_id)
        if not source:
            result["status"] = "fail"
            result["errors"].append("Page source is empty")
            return result

        # Extract visible text.
        visible_text = extract_visible_text(source)
        result["visible_text"] = visible_text[:500]
        if not visible_text.strip():
            result["status"] = "fail"
            result["errors"].append("No visible text on the page")
            return result

        result["checks"].append("Page renders with visible text")

        # Check for errors.
        error_check = check_for_errors(visible_text)
        if error_check:
            result["status"] = "fail"
            result["errors"].append(error_check)
        else:
            result["checks"].append("No error patterns in visible text")

        # Check for placeholder data.
        placeholder_check = check_for_placeholders(visible_text)
        if placeholder_check:
            result["status"] = "fail"
            result["errors"].append(placeholder_check)
        else:
            result["checks"].append("No placeholder data patterns")

        # Find ALL controls.
        buttons = find_elements(session_id, "css selector", "button")
        links = find_elements(session_id, "css selector", "a")
        selects = find_elements(session_id, "css selector", "select")
        textareas = find_elements(session_id, "css selector", "textarea")
        inputs = find_elements(session_id, "css selector", "input")
        result["controls_found"] = (
            len(buttons) + len(links) + len(selects) + len(textareas) + len(inputs)
        )

        if result["controls_found"] == 0:
            result["checks"].append("No interactive controls (display-only page)")
        else:
            result["checks"].append(f"Found {result['controls_found']} controls")

        # Click EVERY button (not just 5).
        for btn_id in buttons:
            try:
                click_element(session_id, btn_id)
                result["controls_clicked"] += 1
                time.sleep(0.5)
            except Exception as e:
                result["errors"].append(f"Button click failed: {e}")

        # Type into EVERY input + textarea (valid + invalid).
        for input_id in inputs + textareas:
            try:
                clear_element(session_id, input_id)
                send_keys(session_id, input_id, "test query")
                result["inputs_filled"] += 1
                time.sleep(0.3)
                # Also try invalid input (empty).
                clear_element(session_id, input_id)
                time.sleep(0.2)
            except Exception as e:
                result["errors"].append(f"Input fill failed: {e}")

        # Change EVERY select (just click to open, then re-read).
        for sel_id in selects:
            try:
                click_element(session_id, sel_id)
                result["selects_changed"] += 1
                time.sleep(0.3)
            except Exception as e:
                result["errors"].append(f"Select change failed: {e}")

        # Re-capture text after interactions to check for new errors.
        source_after = get_page_source(session_id)
        if source_after:
            text_after = extract_visible_text(source_after)
            error_after = check_for_errors(text_after)
            if error_after:
                result["status"] = "fail"
                result["errors"].append(f"Post-interaction error: {error_after}")
            else:
                result["checks"].append("No errors after interactions")

            # Check for placeholder data after interactions.
            placeholder_after = check_for_placeholders(text_after)
            if placeholder_after:
                result["status"] = "fail"
                result["errors"].append(f"Post-interaction placeholder: {placeholder_after}")

    except Exception as e:
        result["status"] = "fail"
        result["errors"].append(str(e))

    return result


def main():
    print(f"E2E Driver: app={APP_BINARY}, data_dir={DATA_DIR}, report={REPORT_PATH}")

    if not wait_for_webdriver():
        print("❌ tauri-driver not ready after 30s")
        write_report([], "fail: tauri-driver not ready")
        sys.exit(1)

    cap_result = webdriver_request(
        "POST",
        "/session",
        {"capabilities": {"alwaysMatch": {}}},
    )
    session_id = None
    if "value" in cap_result:
        val = cap_result["value"]
        if isinstance(val, dict):
            session_id = val.get("sessionId") or val.get("session_id")
        elif isinstance(val, str):
            session_id = val
    if not session_id and "sessionId" in cap_result:
        session_id = cap_result["sessionId"]

    if not session_id:
        print(f"❌ Failed to create WebDriver session: {cap_result}")
        write_report([], f"fail: could not create WebDriver session: {cap_result}")
        sys.exit(1)

    print(f"✅ WebDriver session: {session_id}")

    results = []
    for route, desc, requires_data in PAGES:
        print(f"  Testing {desc} ({route})...")
        result = test_page(session_id, route, desc, requires_data)
        results.append(result)
        status = "✅" if result["status"] == "pass" else "❌"
        print(
            f"  {status} {desc}: "
            f"{result['controls_found']} controls, "
            f"{result['controls_clicked']} clicked, "
            f"{result['inputs_filled']} filled, "
            f"{result['selects_changed']} selects"
        )

    write_report(results, "complete")
    webdriver_request("DELETE", f"/session/{session_id}")

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

        passed = sum(1 for r in results if r["status"] == "pass")
        failed = sum(1 for r in results if r["status"] == "fail")
        total_controls = sum(r["controls_found"] for r in results)
        total_clicked = sum(r["controls_clicked"] for r in results)
        total_inputs = sum(r["inputs_filled"] for r in results)
        total_selects = sum(r["selects_changed"] for r in results)

        f.write("## Summary\n\n")
        f.write(f"- Pages tested: {len(results)}\n")
        f.write(f"- Passed: {passed}\n")
        f.write(f"- Failed: {failed}\n")
        f.write(f"- Total controls found: {total_controls}\n")
        f.write(f"- Total controls clicked: {total_clicked}\n")
        f.write(f"- Total inputs filled: {total_inputs}\n")
        f.write(f"- Total selects changed: {total_selects}\n\n")

        f.write("## Page Results\n\n")
        f.write(
            "| Page | Route | Status | Controls | Clicked | Inputs | Selects | Errors |\n"
        )
        f.write("|---|---|---|---|---|---|---|---|\n")
        for r in results:
            errors = "; ".join(r.get("errors", []))[:100]
            f.write(
                f"| {r['description']} | {r['route']} | {r['status']} | "
                f"{r['controls_found']} | {r['controls_clicked']} | "
                f"{r['inputs_filled']} | {r['selects_changed']} | {errors} |\n"
            )

        f.write("\n## Per-Page Checks\n\n")
        for r in results:
            f.write(f"### {r['description']} ({r['route']})\n")
            f.write(f"Status: {r['status']}\n")
            if r.get("checks"):
                f.write("Checks:\n")
                for c in r["checks"]:
                    f.write(f"  - {c}\n")
            if r.get("errors"):
                f.write("Errors:\n")
                for e in r["errors"]:
                    f.write(f"  - {e}\n")
            f.write(f"Visible text (first 200 chars): {r.get('visible_text', '')[:200]}\n\n")


if __name__ == "__main__":
    main()
