//! `audit` — black-box audit binary (port of the reference's
//! `scripts/audit-phase1.mjs`).
//!
//! M1 scope: scaffold + a small set of M1-relevant checks. The reference
//! audit script is ~1100 lines of HTTP probing of every endpoint; we'll port
//! check categories as the matching milestone lands (M2 sync checks, M3
//! saved-view injection checks, M9 campaign checks, etc.).
//!
//! Output: a JSON findings list to stdout, exit code 0 if no critical issues,
//! 1 otherwise. Same shape as the reference so the owner's tooling is reusable.

use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;

mod checks;

#[derive(Parser)]
#[command(
    name = "audit",
    version,
    about = "SupportOS++ black-box audit binary (port of scripts/audit-phase1.mjs)"
)]
struct Cli {
    /// Path to a packaged app to audit (the directory containing the binary
    /// and resources). M1: the path is checked for existence; M2+ will boot
    /// the binary in demo mode against a throwaway DB.
    #[arg(long)]
    app: String,

    /// Output format: "json" (default) or "text".
    #[arg(long, default_value = "json")]
    format: String,

    /// Fail (exit 1) if any finding has severity "critical".
    #[arg(long, default_value = "true")]
    fail_on_critical: bool,
}

/// One finding produced by an audit check.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Severity: "info", "low", "medium", "high", "critical".
    pub severity: String,
    /// Short human-readable title.
    pub title: String,
    /// Detailed explanation.
    pub detail: String,
    /// The check that produced this finding (e.g. "config_a0").
    pub check: String,
}

impl Finding {
    fn new(severity: &str, title: &str, detail: &str, check: &str) -> Self {
        Self {
            severity: severity.to_string(),
            title: title.to_string(),
            detail: detail.to_string(),
            check: check.to_string(),
        }
    }

    fn info(title: &str, detail: &str, check: &str) -> Self {
        Self::new("info", title, detail, check)
    }

    fn medium(title: &str, detail: &str, check: &str) -> Self {
        Self::new("medium", title, detail, check)
    }

    fn critical(title: &str, detail: &str, check: &str) -> Self {
        Self::new("critical", title, detail, check)
    }
}

/// The aggregate result of an audit run.
#[derive(Debug, Serialize)]
pub struct AuditReport {
    pub app_path: String,
    pub checks_run: u32,
    pub findings: Vec<Finding>,
    pub critical_count: u32,
    pub high_count: u32,
    pub medium_count: u32,
    pub low_count: u32,
    pub info_count: u32,
}

impl AuditReport {
    fn new(app_path: &str) -> Self {
        Self {
            app_path: app_path.to_string(),
            checks_run: 0,
            findings: Vec::new(),
            critical_count: 0,
            high_count: 0,
            medium_count: 0,
            low_count: 0,
            info_count: 0,
        }
    }

    fn record(&mut self, finding: Finding) {
        match finding.severity.as_str() {
            "critical" => self.critical_count += 1,
            "high" => self.high_count += 1,
            "medium" => self.medium_count += 1,
            "low" => self.low_count += 1,
            "info" => self.info_count += 1,
            _ => {}
        }
        self.findings.push(finding);
    }

    fn has_critical(&self) -> bool {
        self.critical_count > 0
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let app_path = PathBuf::from(&cli.app);

    let mut report = AuditReport::new(&cli.app);

    // Always run the path-existence check first — everything else depends on it.
    checks::path_exists::run(&mut report, &app_path);
    report.checks_run += 1;

    // If the path exists, run the rest of the M1 checks.
    let path_ok = report
        .findings
        .iter()
        .filter(|f| f.check == "path_exists")
        .all(|f| f.severity != "critical");

    if path_ok {
        for check in checks::ALL {
            (check.run)(&mut report, &app_path);
            report.checks_run += 1;
        }
    }

    // Output.
    match cli.format.as_str() {
        "json" => {
            let json = serde_json::to_string_pretty(&report)?;
            println!("{json}");
        }
        "text" => {
            println!("SupportOS++ audit — app path: {}", report.app_path);
            println!("Checks run: {}", report.checks_run);
            println!(
                "Findings: {} critical, {} high, {} medium, {} low, {} info",
                report.critical_count,
                report.high_count,
                report.medium_count,
                report.low_count,
                report.info_count
            );
            for f in &report.findings {
                println!(
                    "  [{}] {} — {} (check: {})",
                    f.severity.to_uppercase(),
                    f.title,
                    f.detail,
                    f.check
                );
            }
        }
        other => {
            eprintln!("audit: unknown format {other:?}. Use 'json' or 'text'.");
            std::process::exit(2);
        }
    }

    if cli.fail_on_critical && report.has_critical() {
        std::process::exit(1);
    }
    Ok(())
}

// Suppress unused-import warning for `Path` (used only in function signatures
// of check modules; the type alias keeps the public API stable).
#[allow(dead_code)]
fn _path_type_alias(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_severity_constructors() {
        assert_eq!(Finding::info("a", "b", "c").severity, "info");
        assert_eq!(Finding::medium("a", "b", "c").severity, "medium");
        assert_eq!(Finding::critical("a", "b", "c").severity, "critical");
    }

    #[test]
    fn report_counts_by_severity() {
        let mut r = AuditReport::new("/tmp/x");
        r.record(Finding::critical("c", "d", "x"));
        r.record(Finding::critical("c", "d", "x"));
        r.record(Finding::info("c", "d", "x"));
        r.record(Finding::medium("c", "d", "x"));
        assert_eq!(r.critical_count, 2);
        assert_eq!(r.medium_count, 1);
        assert_eq!(r.info_count, 1);
        assert_eq!(r.high_count, 0);
        assert_eq!(r.low_count, 0);
        assert!(r.has_critical());
    }

    #[test]
    fn empty_report_has_no_critical() {
        let r = AuditReport::new("/tmp/x");
        assert!(!r.has_critical());
    }

    #[test]
    fn report_serializes_to_json() {
        let mut r = AuditReport::new("/tmp/x");
        r.record(Finding::info("ok", "all good", "smoke"));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"app_path\""));
        assert!(json.contains("\"findings\""));
        assert!(json.contains("\"checks_run\""));
    }
}
