//! Shared error type for the SupportOS++ core.
//!
//! Design (A12, D-006):
//! - `Result<T>` is the only result type used across the core.
//! - No `unwrap` / `expect` in production paths (clippy enforces this in CI).
//! - Errors are typed and serializable so the UI can render appropriate states.

use thiserror::Error;

/// The single error type for the SupportOS++ core.
#[derive(Debug, Error)]
pub enum Error {
    /// SQLite returned an error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A migration failed to apply.
    #[error("migration {version} failed: {message}")]
    Migration { version: u32, message: String },

    /// Configuration is invalid or missing.
    #[error("config error: {0}")]
    Config(String),

    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A secret was requested but is not stored.
    #[error("secret not found: {key}")]
    SecretNotFound { key: String },

    /// A webhook signature verification failed.
    #[error("webhook signature verification failed")]
    WebhookSignature,

    /// A webhook event was a duplicate (already processed).
    #[error("webhook event duplicated: {event_id}")]
    WebhookDuplicate { event_id: String },

    /// An OAuth state token was reused (single-use violation).
    #[error("oauth state reused or unknown")]
    OauthStateInvalid,

    /// A required capability is not yet implemented.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    /// JSON (de)serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A request or input failed validation (surfaces as 4xx, never 500).
    #[error("validation error: {0}")]
    Validation(String),

    /// A typed wrapper for any other error.
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// The canonical Result type for the core.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_is_send_sync() {
        // Ensures the error type is usable across threads (Tokio + Tauri requirement).
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Error>();
    }

    #[test]
    fn sqlite_error_converts() {
        let sqlite_err = rusqlite::Error::InvalidQuery;
        let err: Error = sqlite_err.into();
        assert!(matches!(err, Error::Sqlite(_)));
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn migration_error_carries_version() {
        let err = Error::Migration {
            version: 3,
            message: "syntax error".into(),
        };
        let s = err.to_string();
        assert!(s.contains("migration 3"));
        assert!(s.contains("syntax error"));
    }
}
