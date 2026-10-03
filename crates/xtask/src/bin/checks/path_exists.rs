//! Check: does the app path exist?
//!
//! The first check the audit binary runs. Every other check depends on the
//! path existing; if it doesn't, they're all skipped and the run produces a
//! single critical finding.

use std::path::Path;

use super::super::{AuditReport, Finding};

/// Run the path-exists check.
pub fn run(report: &mut AuditReport, app_path: &Path) {
    if app_path.exists() {
        report.record(Finding::info(
            "app path exists",
            &format!("path {} is reachable", app_path.display()),
            "path_exists",
        ));
    } else {
        report.record(Finding::critical(
            "app path does not exist",
            &format!(
                "path {} is not reachable; remaining checks skipped",
                app_path.display()
            ),
            "path_exists",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuditReport;

    #[test]
    fn records_info_when_path_exists() {
        let mut r = AuditReport::new("/tmp");
        // Use a path that exists on every OS: the system temp dir.
        let temp = std::env::temp_dir();
        run(&mut r, &temp);
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, "info");
        assert_eq!(r.findings[0].check, "path_exists");
    }

    #[test]
    fn records_critical_when_path_missing() {
        let mut r = AuditReport::new("/tmp/nope-xyz-12345");
        run(&mut r, Path::new("/tmp/nope-xyz-12345"));
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, "critical");
        assert_eq!(r.findings[0].check, "path_exists");
        assert!(r.has_critical());
    }
}
