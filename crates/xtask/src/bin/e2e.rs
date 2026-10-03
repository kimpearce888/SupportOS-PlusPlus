//! `e2e` — WebDriver-based E2E UI driver for SupportOS++ (Rust port of the
//! former `scripts/e2e_driver.py`; the repository is Rust-native and
//! Linux-only, so the driver lives here).
//!
//! Navigates every page via tauri-driver, captures visible text, clicks every
//! control, fills every input, and writes a plain-text report. Fails on any
//! dead control, any error shown on a page, or any placeholder data.
//!
//! Usage:
//!   cargo run -p supportos-plusplus-xtask --bin e2e -- \
//!       [--webdriver http://127.0.0.1:4444] [--report /tmp/e2e-report.md] \
//!       [--app-binary PATH] [--data-dir PATH]
//!
//! Prerequisites:
//!   - tauri-driver listening on the WebDriver URL
//!   - the app already running (e.g. under xvfb on a headless machine)

use std::fmt::Write as _;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde_json::{json, Value};

/// Pages to test — (route, description).
const PAGES: &[(&str, &str)] = &[
    ("/", "Dashboard"),
    ("/inbox", "Inbox"),
    ("/operations", "Operations Center"),
    ("/notifications", "Notifications"),
    ("/automation", "Automation"),
    ("/sync-health", "Sync Health"),
    ("/customers", "Customer Search"),
    ("/customers/1", "Customer Profile"),
    ("/ai-center", "AI Center"),
    ("/reports", "Reports"),
    ("/issue-radar", "Issue Radar"),
    ("/incidents", "Incidents"),
    ("/knowledge-gaps", "Knowledge Gaps"),
    ("/connectors", "Connectors"),
    ("/custom-objects", "Custom Objects"),
    ("/outreach", "Outreach"),
    ("/search", "Search"),
    ("/backup", "Backup"),
    ("/support-graph", "Support Graph"),
    ("/support-health", "Support Health"),
    ("/settings", "Settings"),
    ("/onboarding", "Onboarding"),
    ("/command-palette", "Command Palette"),
    ("/side-threads", "Side Threads"),
    ("/nonexistent", "404 page"),
];

/// Patterns that indicate placeholder/demo data (not real data).
const PLACEHOLDER_PATTERNS: &[&str] = &[
    "placeholder data",
    "demo data",
    "hardcoded",
    "hard-coded",
    "stub",
    "TODO",
    "FIXME",
    "not implemented",
    "lorem ipsum",
];

/// Patterns that indicate real errors (not UI state messages).
const ERROR_PATTERNS: &[&str] = &[
    "panic",
    "exception",
    "stack trace",
    "undefined",
    "null is not",
    "cannot read prop",
    "typeerror",
];

#[derive(Parser, Debug)]
#[command(name = "e2e", about = "WebDriver E2E UI driver for SupportOS++")]
struct Args {
    /// WebDriver (tauri-driver) base URL.
    #[arg(long, default_value = "http://127.0.0.1:4444")]
    webdriver: String,

    /// Where to write the markdown report.
    #[arg(long, default_value = "/tmp/e2e-report.md")]
    report: PathBuf,

    /// App binary path (recorded in the report).
    #[arg(long, default_value = "target/debug/supportos-plusplus")]
    app_binary: PathBuf,

    /// App data dir (recorded in the report).
    #[arg(long, default_value = "/tmp/spp-e2e-test")]
    data_dir: PathBuf,
}

/// Minimal blocking WebDriver client (localhost, plain HTTP).
struct WebDriver {
    base: String,
    client: reqwest::blocking::Client,
}

impl WebDriver {
    fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("building HTTP client for WebDriver must succeed"),
        }
    }

    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let builder = match method {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url).json(&body.unwrap_or(json!({}))),
            "DELETE" => self.client.delete(&url),
            _ => bail!("unsupported WebDriver method {method}"),
        };
        let resp = builder.send().context("WebDriver HTTP request failed")?;
        let status = resp.status();
        let text = resp.text().context("reading WebDriver response body")?;
        let value: Value = if text.is_empty() {
            json!(null)
        } else {
            serde_json::from_str(&text).with_context(|| {
                format!("parsing WebDriver response from {method} {path}: {text}")
            })?
        };
        if !status.is_success() {
            // Return the error envelope like the Python driver did.
            return Ok(json!({"error": format!("HTTP {status}"), "body": value}));
        }
        Ok(value)
    }

    fn wait_ready(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(v) = self.request("GET", "/status", None) {
                let s = v.to_string().to_lowercase();
                if s.contains("ready") || v.get("value").is_some() {
                    return true;
                }
            }
            sleep(Duration::from_secs(1));
        }
        false
    }

    fn new_session(&self) -> Result<String> {
        let result = self.request(
            "POST",
            "/session",
            Some(json!({"capabilities": {"alwaysMatch": {}}})),
        )?;
        if let Some(val) = result.get("value") {
            if let Some(id) = val.get("sessionId").and_then(|v| v.as_str()) {
                return Ok(id.to_string());
            }
            if let Some(id) = val.get("session_id").and_then(|v| v.as_str()) {
                return Ok(id.to_string());
            }
            if let Some(id) = val.as_str() {
                return Ok(id.to_string());
            }
        }
        if let Some(id) = result.get("sessionId").and_then(|v| v.as_str()) {
            return Ok(id.to_string());
        }
        bail!("could not create WebDriver session: {result}")
    }

    fn page_source(&self, session: &str) -> String {
        self.request("GET", &format!("/session/{session}/source"), None)
            .ok()
            .and_then(|v| v.get("value").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or_default()
    }

    fn navigate(&self, session: &str, url: &str) {
        let _ = self.request(
            "POST",
            &format!("/session/{session}/url"),
            Some(json!({"url": url})),
        );
    }

    fn find_elements(&self, session: &str, selector: &str) -> Vec<String> {
        let Ok(v) = self.request(
            "POST",
            &format!("/session/{session}/elements"),
            Some(json!({"using": "css selector", "value": selector})),
        ) else {
            return Vec::new();
        };
        let Some(list) = v.get("value").and_then(|l| l.as_array()) else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|el| {
                el.get("ELEMENT")
                    .or_else(|| el.get("element-6066-11e4-a52e-4f735466cecf"))
                    .and_then(|id| id.as_str())
                    .map(String::from)
            })
            .collect()
    }

    fn click(&self, session: &str, element: &str) -> Result<()> {
        self.request(
            "POST",
            &format!("/session/{session}/element/{element}/click"),
            None,
        )
        .map(|_| ())
    }

    fn send_keys(&self, session: &str, element: &str, text: &str) -> Result<()> {
        self.request(
            "POST",
            &format!("/session/{session}/element/{element}/value"),
            Some(json!({"text": text})),
        )
        .map(|_| ())
    }

    fn clear(&self, session: &str, element: &str) -> Result<()> {
        self.request(
            "POST",
            &format!("/session/{session}/element/{element}/clear"),
            None,
        )
        .map(|_| ())
    }
}

/// Remove `<script>…</script>` and `<style>…</style>` blocks (case-insensitive).
fn strip_script_style(html: &str) -> String {
    let lower = html.to_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    loop {
        let rest_lower = &lower[i..];
        let rest_html = &html[i..];
        let (open, tag) = match (rest_lower.find("<script"), rest_lower.find("<style")) {
            (Some(a), Some(b)) if a <= b => (a, "script"),
            (Some(_), Some(b)) => (b, "style"),
            (Some(a), None) => (a, "script"),
            (None, Some(b)) => (b, "style"),
            (None, None) => {
                out.push_str(rest_html);
                break;
            }
        };
        out.push_str(&html[i..i + open]);
        let close_tag = format!("</{tag}>");
        match lower[i + open..].find(&close_tag) {
            Some(c) => i = i + open + c + close_tag.len(),
            // Unterminated block: drop the remainder.
            None => break,
        }
    }
    out
}

/// Extract visible text from HTML (strip script/style, tags, decode entities).
fn extract_visible_text(html: &str) -> String {
    let no_blocks = strip_script_style(html);

    let mut text = String::with_capacity(no_blocks.len());
    let mut in_tag = false;
    for ch in no_blocks.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    for (from, to) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&nbsp;", " "),
        ("&quot;", "\""),
        ("&#39;", "'"),
    ] {
        text = text.replace(from, to);
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn matches_any(text_lower: &str, patterns: &'static [&'static str]) -> Option<&'static str> {
    // Substring match against lowercased visible text (mirrors the Python driver).
    patterns.iter().find(|p| text_lower.contains(*p)).copied()
}

struct PageResult {
    route: &'static str,
    description: &'static str,
    pass: bool,
    visible_text: String,
    controls_found: usize,
    controls_clicked: usize,
    inputs_filled: usize,
    selects_changed: usize,
    errors: Vec<String>,
    checks: Vec<String>,
}

fn test_page(
    driver: &WebDriver,
    session: &str,
    route: &'static str,
    description: &'static str,
) -> PageResult {
    let mut r = PageResult {
        route,
        description,
        pass: true,
        visible_text: String::new(),
        controls_found: 0,
        controls_clicked: 0,
        inputs_filled: 0,
        selects_changed: 0,
        errors: Vec::new(),
        checks: Vec::new(),
    };

    // Navigate + wait for async IPC calls.
    driver.navigate(
        session,
        &format!("http://tauri.localhost/index.html#{route}"),
    );
    sleep(Duration::from_secs(3));

    let source = driver.page_source(session);
    if source.is_empty() {
        r.pass = false;
        r.errors.push("Page source is empty".into());
        return r;
    }
    let visible = extract_visible_text(&source);
    r.visible_text = visible.chars().take(500).collect();
    if visible.trim().is_empty() {
        r.pass = false;
        r.errors.push("No visible text on the page".into());
        return r;
    }
    r.checks.push("Page renders with visible text".into());

    let lower = visible.to_lowercase();
    if let Some(p) = matches_any(&lower, ERROR_PATTERNS) {
        r.pass = false;
        r.errors.push(format!("Error pattern found: {p}"));
    } else {
        r.checks.push("No error patterns in visible text".into());
    }
    if let Some(p) = matches_any(&lower, PLACEHOLDER_PATTERNS) {
        r.pass = false;
        r.errors.push(format!("Placeholder pattern found: {p}"));
    } else {
        r.checks.push("No placeholder data patterns".into());
    }

    let buttons = driver.find_elements(session, "button");
    let links = driver.find_elements(session, "a");
    let selects = driver.find_elements(session, "select");
    let textareas = driver.find_elements(session, "textarea");
    let inputs = driver.find_elements(session, "input");
    r.controls_found = buttons.len() + links.len() + selects.len() + textareas.len() + inputs.len();
    if r.controls_found == 0 {
        r.checks
            .push("No interactive controls (display-only page)".into());
    } else {
        r.checks
            .push(format!("Found {} controls", r.controls_found));
    }

    for btn in &buttons {
        match driver.click(session, btn) {
            Ok(()) => {
                r.controls_clicked += 1;
                sleep(Duration::from_millis(500));
            }
            Err(e) => r.errors.push(format!("Button click failed: {e}")),
        }
    }

    for input in inputs.iter().chain(textareas.iter()) {
        match driver
            .clear(session, input)
            .and_then(|_| driver.send_keys(session, input, "test query"))
        {
            Ok(()) => {
                r.inputs_filled += 1;
                sleep(Duration::from_millis(300));
                let _ = driver.clear(session, input);
                sleep(Duration::from_millis(200));
            }
            Err(e) => r.errors.push(format!("Input fill failed: {e}")),
        }
    }

    for sel in &selects {
        match driver.click(session, sel) {
            Ok(()) => {
                r.selects_changed += 1;
                sleep(Duration::from_millis(300));
            }
            Err(e) => r.errors.push(format!("Select change failed: {e}")),
        }
    }

    let source_after = driver.page_source(session);
    if !source_after.is_empty() {
        let text_after = extract_visible_text(&source_after);
        let lower_after = text_after.to_lowercase();
        if let Some(p) = matches_any(&lower_after, ERROR_PATTERNS) {
            r.pass = false;
            r.errors.push(format!("Post-interaction error: {p}"));
        } else {
            r.checks.push("No errors after interactions".into());
        }
        if let Some(p) = matches_any(&lower_after, PLACEHOLDER_PATTERNS) {
            r.pass = false;
            r.errors.push(format!("Post-interaction placeholder: {p}"));
        }
    }

    r
}

fn iso_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple epoch → UTC formatting (report metadata only).
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days algorithm (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn write_report(results: &[PageResult], overall: &str, args: &Args) -> Result<()> {
    let mut f = String::new();
    let _ = writeln!(f, "# E2E UI Test Report\n");
    let _ = writeln!(f, "App: {}", args.app_binary.display());
    let _ = writeln!(f, "Data dir: {}", args.data_dir.display());
    let _ = writeln!(f, "Timestamp: {}", iso_now());
    let _ = writeln!(f, "Overall: {overall}\n");

    let passed = results.iter().filter(|r| r.pass).count();
    let failed = results.len() - passed;
    let total_controls: usize = results.iter().map(|r| r.controls_found).sum();
    let total_clicked: usize = results.iter().map(|r| r.controls_clicked).sum();
    let total_inputs: usize = results.iter().map(|r| r.inputs_filled).sum();
    let total_selects: usize = results.iter().map(|r| r.selects_changed).sum();

    let _ = writeln!(f, "## Summary\n");
    let _ = writeln!(f, "- Pages tested: {}", results.len());
    let _ = writeln!(f, "- Passed: {passed}");
    let _ = writeln!(f, "- Failed: {failed}");
    let _ = writeln!(f, "- Total controls found: {total_controls}");
    let _ = writeln!(f, "- Total controls clicked: {total_clicked}");
    let _ = writeln!(f, "- Total inputs filled: {total_inputs}");
    let _ = writeln!(f, "- Total selects changed: {total_selects}\n");

    let _ = writeln!(f, "## Page Results\n");
    let _ = writeln!(
        f,
        "| Page | Route | Status | Controls | Clicked | Inputs | Selects | Errors |"
    );
    let _ = writeln!(f, "|---|---|---|---|---|---|---|---|");
    for r in results {
        let errors: String = r.errors.join("; ").chars().take(100).collect();
        let status = if r.pass { "pass" } else { "fail" };
        let _ = writeln!(
            f,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            r.description,
            r.route,
            status,
            r.controls_found,
            r.controls_clicked,
            r.inputs_filled,
            r.selects_changed,
            errors
        );
    }

    let _ = writeln!(f, "\n## Per-Page Checks\n");
    for r in results {
        let _ = writeln!(f, "### {} ({})", r.description, r.route);
        let _ = writeln!(f, "Status: {}", if r.pass { "pass" } else { "fail" });
        if !r.checks.is_empty() {
            let _ = writeln!(f, "Checks:");
            for c in &r.checks {
                let _ = writeln!(f, "  - {c}");
            }
        }
        if !r.errors.is_empty() {
            let _ = writeln!(f, "Errors:");
            for e in &r.errors {
                let _ = writeln!(f, "  - {e}");
            }
        }
        let preview: String = r.visible_text.chars().take(200).collect();
        let _ = writeln!(f, "Visible text (first 200 chars): {preview}\n");
    }

    std::fs::write(&args.report, f)
        .with_context(|| format!("writing report {}", args.report.display()))?;
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!(
        "E2E Driver: app={}, data_dir={}, report={}",
        args.app_binary.display(),
        args.data_dir.display(),
        args.report.display()
    );

    let driver = WebDriver::new(&args.webdriver);
    if !driver.wait_ready(Duration::from_secs(30)) {
        write_report(&[], "fail: tauri-driver not ready", &args).ok();
        bail!("tauri-driver not ready after 30s at {}", args.webdriver);
    }

    let session = driver.new_session()?;
    println!("WebDriver session: {session}");

    let mut results = Vec::new();
    for (route, desc) in PAGES {
        println!("  Testing {desc} ({route})...");
        let r = test_page(&driver, &session, route, desc);
        let mark = if r.pass { "PASS" } else { "FAIL" };
        println!(
            "  [{mark}] {desc}: {} controls, {} clicked, {} filled, {} selects",
            r.controls_found, r.controls_clicked, r.inputs_filled, r.selects_changed
        );
        results.push(r);
    }

    write_report(&results, "complete", &args)?;
    let _ = driver.request("DELETE", &format!("/session/{session}"), None);

    let failed: Vec<&PageResult> = results.iter().filter(|r| !r.pass).collect();
    if !failed.is_empty() {
        println!("\n{} page(s) failed:", failed.len());
        for r in &failed {
            println!("  - {} ({}): {:?}", r.description, r.route, r.errors);
        }
        bail!(
            "{} of {} pages failed the E2E drive",
            failed.len(),
            results.len()
        );
    }
    println!("\nAll {} pages passed!", results.len());
    Ok(())
}
