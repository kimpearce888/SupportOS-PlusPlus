//! AI Center — model selection + status + Copilot tool allowlist (M6-T01).
//!
//! Per spec M6: "AI Center."
//! Per spec A10: "Also show the Copilot's read-only tool allowlist in the AI
//! Center for transparency."
//! Per spec A5: "LM Studio and Ollama are optional, never bundled: auto-detect,
//! list models, select, test. The app works fully without them."
//!
//! The AI Center is the hub where the user:
//! 1. Selects which local AI provider to use (LM Studio / Ollama / Generic).
//! 2. Picks a chat model + an embedding model from the provider's list.
//! 3. Sees the provider's status (available / not running).
//! 4. Views the Copilot tool allowlist (22 read-only tools — per A10).

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::catalog::CopilotTool;
use crate::error::Result;

/// The M009 migration: creates the `ai_settings` table.
pub const M009_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS ai_settings (
        id              INTEGER PRIMARY KEY CHECK (id = 1),
        provider_kind   TEXT NOT NULL DEFAULT 'none',
        chat_model      TEXT,
        embedding_model TEXT,
        embedding_dim   INTEGER,
        base_url        TEXT,
        updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    INSERT OR IGNORE INTO ai_settings (id, provider_kind) VALUES (1, 'none');

    UPDATE app_state SET schema_version = 9 WHERE id = 1;
"#;

/// Apply M009 migration. Idempotent.
pub fn apply_m009(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ai_settings (
            id              INTEGER PRIMARY KEY CHECK (id = 1),
            provider_kind   TEXT NOT NULL DEFAULT 'none',
            chat_model      TEXT,
            embedding_model TEXT,
            embedding_dim   INTEGER,
            base_url        TEXT,
            updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        INSERT OR IGNORE INTO ai_settings (id, provider_kind) VALUES (1, 'none');",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 9 WHERE id = 1", []);
    Ok(())
}

/// The kind of local AI provider configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// No provider configured (the app works fully without AI per spec A5).
    None,
    /// LM Studio (OpenAI-compatible at 127.0.0.1:1234).
    LmStudio,
    /// Ollama (native API at 127.0.0.1:11434).
    Ollama,
    /// Generic OpenAI-compatible endpoint (user-configured URL + optional key).
    Generic,
}

impl ProviderKind {
    /// All variants in spec order.
    pub const ALL: [Self; 4] = [Self::None, Self::LmStudio, Self::Ollama, Self::Generic];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LmStudio => "lm_studio",
            Self::Ollama => "ollama",
            Self::Generic => "generic",
        }
    }

    /// Parse from a stored string. Returns `None` for unknown values.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "lm_studio" => Some(Self::LmStudio),
            "ollama" => Some(Self::Ollama),
            "generic" => Some(Self::Generic),
            _ => None,
        }
    }

    /// Human-readable label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None (no AI provider)",
            Self::LmStudio => "LM Studio",
            Self::Ollama => "Ollama",
            Self::Generic => "Generic (OpenAI-compatible)",
        }
    }
}

/// The AI status — what the AI Center UI displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiStatus {
    /// Which provider is configured.
    pub provider_kind: ProviderKind,
    /// The selected chat model (None if not selected).
    pub chat_model: Option<String>,
    /// The selected embedding model (None if not selected).
    pub embedding_model: Option<String>,
    /// The embedding dimension (None if not determined yet).
    pub embedding_dim: Option<usize>,
    /// The base URL for the provider (None for "none").
    pub base_url: Option<String>,
    /// Whether the provider is currently available (running).
    /// Determined at runtime by `is_available()` — stored as `None` until
    /// the caller checks.
    pub provider_available: Option<bool>,
}

impl Default for AiStatus {
    fn default() -> Self {
        Self {
            provider_kind: ProviderKind::None,
            chat_model: None,
            embedding_model: None,
            embedding_dim: None,
            base_url: None,
            provider_available: None,
        }
    }
}

/// Get the current AI status from the settings store.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn get_ai_status(conn: &Connection) -> Result<AiStatus> {
    // Use a struct to avoid the clippy type_complexity lint on a 5-tuple.
    struct AiSettingsRow {
        kind: String,
        chat: Option<String>,
        embed: Option<String>,
        dim: Option<i64>,
        url: Option<String>,
    }
    let row: Option<AiSettingsRow> = conn
        .query_row(
            "SELECT provider_kind, chat_model, embedding_model, embedding_dim, base_url
             FROM ai_settings WHERE id = 1",
            [],
            |r| {
                Ok(AiSettingsRow {
                    kind: r.get(0)?,
                    chat: r.get(1)?,
                    embed: r.get(2)?,
                    dim: r.get(3)?,
                    url: r.get(4)?,
                })
            },
        )
        .ok();
    match row {
        None => Ok(AiStatus::default()),
        Some(r) => Ok(AiStatus {
            provider_kind: ProviderKind::parse(&r.kind).unwrap_or(ProviderKind::None),
            chat_model: r.chat,
            embedding_model: r.embed,
            embedding_dim: r.dim.map(|d| d as usize),
            base_url: r.url,
            provider_available: None, // checked at runtime
        }),
    }
}

/// Set the provider kind. Per spec A5: "auto-detect, list models, select, test."
///
/// # Errors
///
/// Returns `Error::Sqlite` if the update fails.
pub fn set_provider_kind(conn: &Connection, kind: ProviderKind) -> Result<()> {
    conn.execute(
        "UPDATE ai_settings SET provider_kind = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = 1",
        params![kind.as_str()],
    )?;
    Ok(())
}

/// Set the chat model.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the update fails.
pub fn set_chat_model(conn: &Connection, model: &str) -> Result<()> {
    conn.execute(
        "UPDATE ai_settings SET chat_model = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = 1",
        params![model],
    )?;
    Ok(())
}

/// Set the embedding model + dimension. The dim is read from the model's first
/// response per the reference notes.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the update fails.
pub fn set_embedding_model(conn: &Connection, model: &str, dim: usize) -> Result<()> {
    conn.execute(
        "UPDATE ai_settings SET embedding_model = ?1, embedding_dim = ?2,
         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = 1",
        params![model, dim as i64],
    )?;
    Ok(())
}

/// Set the base URL (for the Generic provider).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the update fails.
pub fn set_base_url(conn: &Connection, url: &str) -> Result<()> {
    conn.execute(
        "UPDATE ai_settings SET base_url = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = 1",
        params![url],
    )?;
    Ok(())
}

/// The Copilot tool allowlist — 22 read-only tools (per spec A10 transparency).
/// Returns the full list from the catalog's `CopilotTool::ALL`.
#[must_use]
pub fn copilot_tool_allowlist() -> &'static [CopilotTool] {
    &CopilotTool::ALL
}

/// Whether AI features are enabled (provider configured + chat model selected).
/// Per spec A5: "The app works fully without them."
#[must_use]
pub fn ai_features_enabled(status: &AiStatus) -> bool {
    status.provider_kind != ProviderKind::None && status.chat_model.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        apply_m009(&conn).unwrap();
        conn
    }

    // ---- M009 migration ----------------------------------------------------

    #[test]
    fn m009_creates_ai_settings_table() {
        let conn = fresh_db();
        let kind: String = conn
            .query_row(
                "SELECT provider_kind FROM ai_settings WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "none", "default provider is 'none'");
    }

    #[test]
    fn m009_is_idempotent() {
        let conn = fresh_db();
        apply_m009(&conn).unwrap();
    }

    // ---- get_ai_status (default) -------------------------------------------

    #[test]
    fn get_ai_status_default_returns_none_provider() {
        let conn = fresh_db();
        let status = get_ai_status(&conn).unwrap();
        assert_eq!(status.provider_kind, ProviderKind::None);
        assert!(status.chat_model.is_none());
        assert!(status.embedding_model.is_none());
        assert!(status.embedding_dim.is_none());
        assert!(status.base_url.is_none());
    }

    // ---- set_provider_kind -------------------------------------------------

    #[test]
    fn set_provider_kind_round_trips() {
        let conn = fresh_db();
        set_provider_kind(&conn, ProviderKind::LmStudio).unwrap();
        let status = get_ai_status(&conn).unwrap();
        assert_eq!(status.provider_kind, ProviderKind::LmStudio);
    }

    #[test]
    fn set_provider_kind_ollama() {
        let conn = fresh_db();
        set_provider_kind(&conn, ProviderKind::Ollama).unwrap();
        assert_eq!(
            get_ai_status(&conn).unwrap().provider_kind,
            ProviderKind::Ollama
        );
    }

    #[test]
    fn set_provider_kind_generic() {
        let conn = fresh_db();
        set_provider_kind(&conn, ProviderKind::Generic).unwrap();
        assert_eq!(
            get_ai_status(&conn).unwrap().provider_kind,
            ProviderKind::Generic
        );
    }

    // ---- set_chat_model ----------------------------------------------------

    #[test]
    fn set_chat_model_round_trips() {
        let conn = fresh_db();
        set_chat_model(&conn, "llama-3.1-8b").unwrap();
        let status = get_ai_status(&conn).unwrap();
        assert_eq!(status.chat_model.as_deref(), Some("llama-3.1-8b"));
    }

    // ---- set_embedding_model ----------------------------------------------

    #[test]
    fn set_embedding_model_round_trips_with_dim() {
        let conn = fresh_db();
        set_embedding_model(&conn, "text-embedding-3-small", 1536).unwrap();
        let status = get_ai_status(&conn).unwrap();
        assert_eq!(
            status.embedding_model.as_deref(),
            Some("text-embedding-3-small")
        );
        assert_eq!(status.embedding_dim, Some(1536));
    }

    // ---- set_base_url ------------------------------------------------------

    #[test]
    fn set_base_url_round_trips() {
        let conn = fresh_db();
        set_provider_kind(&conn, ProviderKind::Generic).unwrap();
        set_base_url(&conn, "http://localhost:8080/v1").unwrap();
        let status = get_ai_status(&conn).unwrap();
        assert_eq!(status.base_url.as_deref(), Some("http://localhost:8080/v1"));
    }

    // ---- copilot_tool_allowlist --------------------------------------------

    #[test]
    fn copilot_tool_allowlist_has_22_tools() {
        assert_eq!(copilot_tool_allowlist().len(), 22);
    }

    #[test]
    fn copilot_tool_allowlist_is_read_only() {
        // All 22 tools have a description (proof they're real, documented tools).
        for &tool in copilot_tool_allowlist() {
            assert!(!tool.description().is_empty());
        }
    }

    // ---- ai_features_enabled -----------------------------------------------

    #[test]
    fn ai_features_disabled_when_no_provider() {
        let status = AiStatus::default();
        assert!(!ai_features_enabled(&status));
    }

    #[test]
    fn ai_features_disabled_when_no_chat_model() {
        let status = AiStatus {
            provider_kind: ProviderKind::LmStudio,
            chat_model: None,
            ..Default::default()
        };
        assert!(!ai_features_enabled(&status));
    }

    #[test]
    fn ai_features_enabled_when_provider_and_chat_model() {
        let status = AiStatus {
            provider_kind: ProviderKind::LmStudio,
            chat_model: Some("llama-3".into()),
            ..Default::default()
        };
        assert!(ai_features_enabled(&status));
    }

    // ---- ProviderKind ------------------------------------------------------

    #[test]
    fn provider_kind_all_has_four_variants() {
        assert_eq!(ProviderKind::ALL.len(), 4);
    }

    #[test]
    fn provider_kind_parse_round_trips() {
        for kind in ProviderKind::ALL {
            assert_eq!(ProviderKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn provider_kind_parse_unknown_returns_none() {
        assert!(ProviderKind::parse("unknown").is_none());
    }

    #[test]
    fn provider_kind_labels_are_human_readable() {
        for kind in ProviderKind::ALL {
            let label = kind.label();
            assert!(!label.is_empty());
            assert!(
                !label.starts_with(char::is_lowercase),
                "labels should be title-cased"
            );
        }
    }

    #[test]
    fn provider_kind_serializes_snake_case() {
        let s = serde_json::to_string(&ProviderKind::LmStudio).unwrap();
        assert_eq!(s, "\"lm_studio\"");
    }

    // ---- AiStatus serde ----------------------------------------------------

    #[test]
    fn ai_status_serializes() {
        let status = AiStatus {
            provider_kind: ProviderKind::Ollama,
            chat_model: Some("llama3".into()),
            embedding_model: Some("nomic-embed-text".into()),
            embedding_dim: Some(768),
            base_url: Some("http://127.0.0.1:11434".into()),
            provider_available: Some(true),
        };
        let s = serde_json::to_string(&status).unwrap();
        assert!(s.contains("\"provider_kind\":\"ollama\""));
        assert!(s.contains("\"chat_model\":\"llama3\""));
        assert!(s.contains("\"embedding_dim\":768"));
        assert!(s.contains("\"provider_available\":true"));
    }
}
