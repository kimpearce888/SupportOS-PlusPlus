//! Configuration loader for SupportOS++.
//!
//! No `.env` files. No environment variables for app behaviour (the spec bans a Node-style env-driven config).
//! All configuration lives in the SQLite `application_settings` table; the user edits it via the Settings UI.
//! The only env vars read are `SPP_DATA_DIR` (override the data folder for tests/dev) and `RUST_LOG` (tracing).

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The full application configuration.
///
/// Values here are the *defaults*; the actual values live in the DB and override these at runtime.
/// The user-facing settings UI edits the DB; this struct is the schema for what's stored.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppConfig {
    /// The user's data directory (SQLite DB, attachments, backups).
    pub data_dir: PathBuf,
    /// Whether demo mode is on (no real Help Scout credentials).
    pub demo_mode: bool,
    /// Help Scout OAuth config (masked on read when present).
    pub helpscout: HelpScoutConfig,
    /// Local AI provider config (LM Studio).
    pub ai: AiConfig,
    /// Sync interval in minutes (default 5, per A9).
    pub sync_interval_minutes: u32,
    /// Loopback listener port (0 = pick at startup; persisted after first run).
    pub loopback_port: u16,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HelpScoutConfig {
    /// OAuth client id (secret-adjacent: redacted on read).
    pub client_id: String,
    /// OAuth client secret (redacted on read).
    #[serde(skip_serializing, default)]
    pub client_secret: String,
    /// OAuth redirect URI; constructed from the loopback port.
    pub redirect_uri: String,
    /// Webhook secret (redacted on read).
    #[serde(skip_serializing, default)]
    pub webhook_secret: String,
    /// Docs API key (separate from OAuth; redacted on read).
    #[serde(skip_serializing, default)]
    pub docs_api_key: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AiConfig {
    /// "lmstudio" | "ollama" | "generic" | "none".
    pub provider: String,
    /// Base URL of the provider, e.g. `http://127.0.0.1:1234/v1`.
    pub base_url: String,
    /// Selected chat model id (empty = not picked).
    pub chat_model: String,
    /// Selected embedding model id (empty = not picked).
    pub embedding_model: String,
    /// Timeout in milliseconds for any single AI request.
    pub timeout_ms: u32,
    /// Max concurrent AI requests.
    pub concurrency: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            demo_mode: false,
            helpscout: HelpScoutConfig::default(),
            ai: AiConfig::default(),
            sync_interval_minutes: 5,
            loopback_port: 0,
        }
    }
}

impl HelpScoutConfig {
    /// Redacted view for the UI / Tauri IPC responses. Never includes secrets.
    pub fn redacted(&self) -> HelpScoutConfig {
        HelpScoutConfig {
            client_id: redact_string(&self.client_id),
            client_secret: String::new(), // never sent to UI
            redirect_uri: self.redirect_uri.clone(),
            webhook_secret: String::new(),
            docs_api_key: String::new(),
        }
    }
}

/// Compute the default Linux data directory.
///
/// - Linux: `${XDG_DATA_HOME:-~/.local/share}/supportos-plusplus`
///
/// The port is Linux-only: no Windows/macOS branches exist.
/// Overridable via `SPP_DATA_DIR` for tests and dev.
pub fn default_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SPP_DATA_DIR") {
        return PathBuf::from(dir);
    }

    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty());
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let root = base
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".local").join("share"));
    root.join("supportos-plusplus")
}

/// Ensure a directory exists (create it if missing).
pub fn ensure_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(Error::Io)?;
    }
    Ok(())
}

/// Replace a string with a redacted form for UI display.
fn redact_string(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    if s.len() <= 4 {
        return "••••".into();
    }
    let head: usize = 2.min(s.len() / 4);
    let tail: usize = 2.min(s.len() / 4);
    format!("{}••••{}", &s[..head], &s[s.len() - tail..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_values() {
        let c = AppConfig::default();
        assert_eq!(c.sync_interval_minutes, 5);
        assert!(!c.demo_mode);
        assert_eq!(c.loopback_port, 0);
    }

    #[test]
    fn redact_short_string() {
        assert_eq!(redact_string(""), "");
        assert_eq!(redact_string("ab"), "••••");
    }

    #[test]
    fn redact_long_string_keeps_head_tail() {
        let r = redact_string("abcdefghij");
        assert!(r.starts_with("ab"));
        assert!(r.ends_with("ij"));
        assert!(r.contains("••••"));
    }

    #[test]
    fn helpscout_redacted_drops_secrets() {
        let h = HelpScoutConfig {
            client_id: "client_1234567890".into(),
            client_secret: "super_secret_value".into(),
            webhook_secret: "wh_secret".into(),
            ..Default::default()
        };
        let r = h.redacted();
        assert!(r.client_id.starts_with("cl"));
        assert!(r.client_secret.is_empty());
        assert!(r.webhook_secret.is_empty());
    }

    #[test]
    fn data_dir_respects_env() {
        std::env::set_var("SPP_DATA_DIR", "/tmp/spp-test-data-dir");
        let d = default_data_dir();
        assert!(d.to_string_lossy().contains("spp-test-data-dir"));
        std::env::remove_var("SPP_DATA_DIR");
    }
}
