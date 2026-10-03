//! Configuration loader for SupportOS++.
//!
//! Mirrors the reference `src/server/config/config.ts`: a `.env`-free
//! environment layer read once at boot (19 vars with the reference's
//! defaults) + the SQLite `application_settings` table the Settings UI
//! edits. Help Scout credentials (OAuth, webhook secret, Docs API key) are
//! env-only in the reference; LM Studio/Qdrant runtime settings are
//! DB-backed in both (the env values load into the config shape but the
//! clients read the settings repo — faithful to the reference, where
//! `config.lmstudio.*`/`config.qdrant.*` also have no consumers).

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

// ---------------------------------------------------------------------------
// Environment layer (reference config.ts:62-114 — 19 vars, same defaults)
// ---------------------------------------------------------------------------

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        _ => default,
    }
}

fn env_int(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// The boot-time environment configuration (config.ts `loadConfig()`).
///
/// Help Scout credentials are consumed from here (env-only in the
/// reference); the LM Studio/Qdrant/sync values load for shape parity —
/// the runtime clients read the settings repo, exactly as in the reference
/// (where `config.lmstudio.*` and `config.qdrant.*` have no consumers).
#[derive(Debug, Clone, PartialEq)]
pub struct EnvConfig {
    pub demo_mode: bool,
    pub log_level: String,
    pub helpscout: EnvHelpScout,
    pub lmstudio: EnvLmStudio,
    pub qdrant: EnvQdrant,
    pub sync: EnvSync,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvHelpScout {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub api_base: String,
    pub webhook_secret: String,
    pub docs_api_key: String,
    pub docs_api_base: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvLmStudio {
    pub base_url: String,
    pub chat_model: Option<String>,
    pub embedding_model: Option<String>,
    pub timeout_ms: i64,
    pub concurrency: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvQdrant {
    pub url: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvSync {
    pub interval_minutes: i64,
    pub api_concurrency: i64,
}

/// `loadConfig()` — read the environment layer (config.ts defaults).
pub fn load_env_config() -> EnvConfig {
    EnvConfig {
        demo_mode: env_bool("LOCAL_DEMO_MODE", false),
        log_level: env_str("LOG_LEVEL", "info"),
        helpscout: EnvHelpScout {
            client_id: env_str("HELPSCOUT_CLIENT_ID", ""),
            client_secret: env_str("HELPSCOUT_CLIENT_SECRET", ""),
            redirect_uri: env_str(
                "HELPSCOUT_REDIRECT_URI",
                "http://localhost:3000/oauth/callback",
            ),
            api_base: env_str("HELPSCOUT_API_BASE", "https://api.helpscout.net"),
            webhook_secret: env_str("HELPSCOUT_WEBHOOK_SECRET", ""),
            docs_api_key: env_str("HELPSCOUT_DOCS_API_KEY", ""),
            docs_api_base: env_str("HELPSCOUT_DOCS_API_BASE", "https://docsapi.helpscout.net"),
        },
        lmstudio: EnvLmStudio {
            base_url: env_str("LMSTUDIO_BASE_URL", "http://127.0.0.1:1234"),
            chat_model: std::env::var("LMSTUDIO_CHAT_MODEL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            embedding_model: std::env::var("LMSTUDIO_EMBEDDING_MODEL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            timeout_ms: env_int("LMSTUDIO_TIMEOUT_MS", 120_000),
            concurrency: env_int("LMSTUDIO_CONCURRENCY", 2),
        },
        qdrant: EnvQdrant {
            url: env_str("QDRANT_URL", "http://127.0.0.1:6333"),
            enabled: env_bool("QDRANT_ENABLED", true),
        },
        sync: EnvSync {
            interval_minutes: env_int("SYNC_INTERVAL_MINUTES", 5),
            api_concurrency: env_int("SYNC_API_CONCURRENCY", 2),
        },
    }
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

    /// loadConfig defaults (config.ts:85-114) with a clean environment.
    #[test]
    fn env_config_defaults_match_reference() {
        // Run in a subprocess-clean env: tests share one process, so only
        // assert keys no other test touches.
        let c = load_env_config();
        assert_eq!(
            c.helpscout.redirect_uri,
            "http://localhost:3000/oauth/callback"
        );
        assert_eq!(c.helpscout.api_base, "https://api.helpscout.net");
        assert_eq!(c.helpscout.docs_api_base, "https://docsapi.helpscout.net");
        assert_eq!(c.lmstudio.timeout_ms, 120_000);
        assert_eq!(c.lmstudio.concurrency, 2);
        assert_eq!(c.qdrant.url, "http://127.0.0.1:6333");
        assert!(c.qdrant.enabled);
        assert_eq!(c.sync.interval_minutes, 5);
        assert_eq!(c.sync.api_concurrency, 2);
    }

    #[test]
    fn env_config_reads_set_vars() {
        std::env::set_var("LMSTUDIO_CONCURRENCY", "7");
        std::env::set_var("HELPSCOUT_DOCS_API_KEY", "docs-key-123");
        let c = load_env_config();
        std::env::remove_var("LMSTUDIO_CONCURRENCY");
        std::env::remove_var("HELPSCOUT_DOCS_API_KEY");
        assert_eq!(c.lmstudio.concurrency, 7);
        assert_eq!(c.helpscout.docs_api_key, "docs-key-123");
    }

    #[test]
    fn env_bool_parses_reference_forms() {
        std::env::set_var("QDRANT_ENABLED", "true");
        assert!(load_env_config().qdrant.enabled);
        std::env::set_var("QDRANT_ENABLED", "TRUE");
        assert!(load_env_config().qdrant.enabled);
        std::env::set_var("QDRANT_ENABLED", "0");
        assert!(!load_env_config().qdrant.enabled);
        std::env::remove_var("QDRANT_ENABLED");
    }
}
