//! Check: does the packaged app's `tauri.conf.json` meet spec amendment A0?
//!
//! Reuses the workspace's `verify_config` module so the same assertions run
//! in `cargo xtask verify-config` (CI step) and in `cargo xtask audit` (black-box
//! check). One source of truth per A12.

use std::path::{Path, PathBuf};

use super::super::{AuditReport, Finding};

/// Run the config-A0 check.
pub fn run(report: &mut AuditReport, app_path: &Path) {
    // Try to find tauri.conf.json relative to the app path.
    let conf = locate_tauri_config(app_path);
    let Some(conf_path) = conf else {
        report.record(Finding::medium(
            "tauri.conf.json not found",
            &format!(
                "could not locate tauri.conf.json under {}",
                app_path.display()
            ),
            "config_a0",
        ));
        return;
    };

    match spp_xtask::verify_config::check(&conf_path) {
        Ok(violations) if violations.is_empty() => {
            report.record(Finding::info(
                "tauri.conf.json meets A0",
                &format!(
                    "{} — all spec amendment A0 mandates satisfied",
                    conf_path.display()
                ),
                "config_a0",
            ));
        }
        Ok(violations) => {
            for v in violations {
                report.record(Finding::medium("A0 violation", &v, "config_a0"));
            }
        }
        Err(e) => {
            report.record(Finding::medium(
                "tauri.conf.json parse failed",
                &format!("could not parse {}: {e}", conf_path.display()),
                "config_a0",
            ));
        }
    }
}

/// Locate `tauri.conf.json` in a few well-known positions relative to `app_path`.
fn locate_tauri_config(app_path: &Path) -> Option<PathBuf> {
    // If the user passed the file itself, use it.
    if app_path.is_file() && app_path.file_name()?.to_str()? == "tauri.conf.json" {
        return Some(app_path.to_path_buf());
    }

    // Common locations under a packaged app path.
    let candidates = [
        app_path.join("tauri.conf.json"),
        app_path.join("resources/tauri.conf.json"),
        app_path.join("crates/app/src-tauri/tauri.conf.json"),
    ];
    candidates.into_iter().find(|c| c.exists())
}

#[cfg(test)]
mod tests {
    use super::super::AuditReport;
    use super::*;

    #[test]
    fn records_medium_when_config_missing() {
        let mut r = AuditReport::new("/tmp/nonexistent-audit-path");
        run(&mut r, Path::new("/tmp/nonexistent-audit-path"));
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, "medium");
        assert_eq!(r.findings[0].check, "config_a0");
    }

    #[test]
    fn records_info_when_config_meets_spec() {
        // Use the workspace's real tauri.conf.json — which the verify_config
        // tests already assert meets A0.
        let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let conf_path = workspace_root
            .join("crates")
            .join("app")
            .join("src-tauri")
            .join("tauri.conf.json");
        if !conf_path.exists() {
            eprintln!("skipping test: {} not found", conf_path.display());
            return;
        }
        let mut r = AuditReport::new(&conf_path.display().to_string());
        run(&mut r, &conf_path);
        // Should produce at least one info finding.
        assert!(r.findings.iter().any(|f| f.severity == "info"));
    }
}
