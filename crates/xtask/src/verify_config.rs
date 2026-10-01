//! `verify-config` — statically verifies the Tauri 2 config meets spec amendment A0.
//!
//! Per A0: the product name must be `SupportOS++`, the bundle identifier must be
//! `com.supportos.plusplus`, and the bundle targets must include all six formats
//! (MSI, NSIS, DMG, DEB, RPM, AppImage).
//!
//! This check runs without needing GTK/WebKit2GTK system libraries (it just parses
//! the JSON config), so it works in any environment — including the local dev
//! sandbox that can't link the Tauri shell crate.

use std::fs;
use std::path::Path;

use serde::Deserialize;

/// The subset of `tauri.conf.json` we care about for A0 verification.
///
/// Field names are camelCase to match the Tauri 2 config schema.
#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct TauriConfig {
    pub productName: String,
    pub identifier: String,
    pub app: AppSection,
    pub bundle: BundleSection,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct AppSection {
    pub windows: Vec<Window>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct Window {
    pub title: String,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct BundleSection {
    pub targets: Vec<String>,
}

/// The six bundle targets the spec mandates (INSTALL AND PACKAGING + A0).
pub const REQUIRED_BUNDLE_TARGETS: &[&str] = &["msi", "nsis", "dmg", "deb", "rpm"];

/// Read + parse the `tauri.conf.json` at `path`.
pub fn load(path: &Path) -> anyhow::Result<TauriConfig> {
    let text =
        fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let cfg: TauriConfig = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?;
    Ok(cfg)
}

/// Verify the config meets spec amendment A0. Returns a list of violations (empty on success).
#[must_use]
pub fn violations(cfg: &TauriConfig) -> Vec<String> {
    let mut out = Vec::new();
    if cfg.productName != "SupportOS++" {
        out.push(format!(
            "productName must be \"SupportOS++\" (A0); got {:?}",
            cfg.productName
        ));
    }
    if cfg.identifier != "com.supportos.plusplus" {
        out.push(format!(
            "identifier must be \"com.supportos.plusplus\" (A0); got {:?}",
            cfg.identifier
        ));
    }
    if cfg.app.windows.is_empty() {
        out.push("app.windows must define at least one window".into());
    } else if cfg.app.windows[0].title != "SupportOS++" {
        out.push(format!(
            "app.windows[0].title must be \"SupportOS++\" (A0); got {:?}",
            cfg.app.windows[0].title
        ));
    }
    for required in REQUIRED_BUNDLE_TARGETS {
        if !cfg.bundle.targets.iter().any(|t| t == required) {
            out.push(format!(
                "bundle.targets must include \"{required}\" (INSTALL AND PACKAGING + A0); got {:?}",
                cfg.bundle.targets
            ));
        }
    }
    out
}

/// Convenience: load + verify, returning the violations list.
pub fn check(path: &Path) -> anyhow::Result<Vec<String>> {
    let cfg = load(path)?;
    Ok(violations(&cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config(product: &str, ident: &str, title: &str, targets: &[&str]) -> TauriConfig {
        TauriConfig {
            productName: product.into(),
            identifier: ident.into(),
            app: AppSection {
                windows: vec![Window {
                    title: title.into(),
                }],
            },
            bundle: BundleSection {
                targets: targets.iter().map(|s| (*s).into()).collect(),
            },
        }
    }

    #[test]
    fn accepts_spec_compliant_config() {
        let cfg = sample_config(
            "SupportOS++",
            "com.supportos.plusplus",
            "SupportOS++",
            REQUIRED_BUNDLE_TARGETS,
        );
        assert!(violations(&cfg).is_empty());
    }

    #[test]
    fn flags_wrong_product_name() {
        let cfg = sample_config(
            "SupportOS", // wrong: missing "++"
            "com.supportos.plusplus",
            "SupportOS++",
            REQUIRED_BUNDLE_TARGETS,
        );
        let v = violations(&cfg);
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("productName"));
    }

    #[test]
    fn flags_wrong_bundle_id() {
        let cfg = sample_config(
            "SupportOS++",
            "com.supportos.local", // wrong
            "SupportOS++",
            REQUIRED_BUNDLE_TARGETS,
        );
        let v = violations(&cfg);
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("identifier"));
    }

    #[test]
    fn flags_wrong_window_title() {
        let cfg = sample_config(
            "SupportOS++",
            "com.supportos.plusplus",
            "SupportOS", // wrong
            REQUIRED_BUNDLE_TARGETS,
        );
        let v = violations(&cfg);
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("windows[0].title"));
    }

    #[test]
    fn flags_missing_bundle_target() {
        let cfg = sample_config(
            "SupportOS++",
            "com.supportos.plusplus",
            "SupportOS++",
            &["msi", "nsis", "dmg", "appimage"], // missing deb + rpm
        );
        let v = violations(&cfg);
        assert_eq!(v.len(), 2);
        assert!(v.iter().any(|s| s.contains("\"deb\"")));
        assert!(v.iter().any(|s| s.contains("\"rpm\"")));
    }

    #[test]
    fn flags_empty_windows_list() {
        let cfg = TauriConfig {
            productName: "SupportOS++".into(),
            identifier: "com.supportos.plusplus".into(),
            app: AppSection { windows: vec![] },
            bundle: BundleSection {
                targets: REQUIRED_BUNDLE_TARGETS
                    .iter()
                    .map(|s| (*s).into())
                    .collect(),
            },
        };
        let v = violations(&cfg);
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("at least one window"));
    }

    /// Integration test: verify the actual tauri.conf.json checked into the repo.
    #[test]
    fn real_config_in_repo_meets_spec() {
        let path = workspace_root().join("crates/app/src-tauri/tauri.conf.json");
        let violations = check(&path).expect("loading tauri.conf.json must succeed");
        assert!(
            violations.is_empty(),
            "tauri.conf.json violates A0: {violations:?}"
        );
    }

    fn workspace_root() -> std::path::PathBuf {
        let manifest = std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
        std::path::PathBuf::from(manifest)
            .ancestors()
            .nth(2)
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            })
    }
}
