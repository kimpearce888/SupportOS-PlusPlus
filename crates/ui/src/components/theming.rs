//! Theming tokens — the Rust-side mirror of the CSS custom properties in
//! `crates/ui/styles/app.css`. Used by components that need to pick a color
//! by name (e.g. severity buckets: info / warning / critical) so the choice
//! is type-safe and refactored in one place.
//!
//! Per A12: type system makes wrong states impossible. A `Severity` is an
//! enum, not a string; the CSS class is derived from the variant.

/// The three severity buckets used by the Operations Center tiles
/// (matches `OperationsTileKey::severity` in the core catalog).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Informational — no action needed.
    Info,
    /// Worth attention — e.g. SLA at risk.
    Warning,
    /// Time-sensitive — e.g. SLA breached, sync failure.
    Critical,
}

impl Severity {
    /// The CSS class suffix for this severity (used as `spp-state--{suffix}`
    /// or `spp-tile--{suffix}`).
    #[must_use]
    pub fn class_suffix(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// The CSS variable name for this severity's foreground color
    /// (e.g. `var(--spp-critical)`).
    #[must_use]
    pub fn color_var(self) -> &'static str {
        match self {
            Self::Info => "var(--spp-info)",
            Self::Warning => "var(--spp-warning)",
            Self::Critical => "var(--spp-critical)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_suffix_matches_expected_strings() {
        assert_eq!(Severity::Info.class_suffix(), "info");
        assert_eq!(Severity::Warning.class_suffix(), "warning");
        assert_eq!(Severity::Critical.class_suffix(), "critical");
    }

    #[test]
    fn color_var_uses_spp_namespace() {
        for s in [Severity::Info, Severity::Warning, Severity::Critical] {
            let v = s.color_var();
            assert!(v.starts_with("var(--spp-"));
            assert!(v.ends_with(')'));
        }
    }

    #[test]
    fn severity_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Severity>();
    }
}
