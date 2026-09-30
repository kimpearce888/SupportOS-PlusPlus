//! Audit check modules. Each module exports a `run(report, app_path)` function
//! that records its findings. The `Check` struct in [`ALL`] makes checks
//! discoverable and runnable by name.

use std::path::Path;

use super::AuditReport;

pub mod config_a0;
pub mod path_exists;

/// One check registered in the catalog.
pub struct Check {
    /// The short name used by `audit --app PATH --category <name>`.
    /// (Reserved for the future `--category` flag; M1 always runs all checks.)
    #[allow(dead_code)]
    pub name: &'static str,
    /// The function that runs the check and records findings.
    pub run: fn(&mut AuditReport, &Path),
}

/// All available checks. Add new checks here as they're implemented per milestone.
pub const ALL: &[Check] = &[Check {
    name: "config_a0",
    run: config_a0::run,
}];
