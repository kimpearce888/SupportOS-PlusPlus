//! OAuth token storage — the `oauth_tokens` table helpers.
//!
//! SY-07 (audit M21) cleanup: this module previously carried the M2-era
//! Tauri-loopback OAuth machinery — `OAuthConfig::authorize_url` (a wire
//! shape the reference never sends), `handle_callback`, `CallbackResult`
//! and a placeholder `exchange_code` that only ever returned a fake token
//! for the literal code `"test_code"` — the dead legacy path the audit
//! flagged (oauth.rs:200-247). The live OAuth flow lives on the real HTTP
//! server since the T9 port: `http/routes/oauth.rs` serves
//! `/api/oauth/authorize-url`, `/api/oauth/client-credentials`,
//! `/api/oauth/status`, `/api/oauth/disconnect` and `/oauth/callback`
//! (single-use CSRF state in `application_settings`, code exchange through
//! `RealHelpScoutProvider::exchange_code`) — reference
//! `routes/sync.ts:269-385` + `authService.ts`.
//!
//! What remains here are the small `oauth_tokens` table helpers used by
//! `conversation_ops::require_auth`'s legacy-DB fallback (databases created
//! before the table grew the `account`/`revoked` columns).

use rusqlite::{params, Connection};

use crate::error::Result;

/// The OAuth token response from Help Scout.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub token_type: String,
    pub scope: Option<String>,
}

// ---------------------------------------------------------------------------
// Token storage (SQLite)
// ---------------------------------------------------------------------------

/// Store the OAuth token in the `oauth_tokens` table. Upserts the single row
/// (id=1). The old token (if any) is replaced.
pub fn store_token(conn: &Connection, token: &OAuthToken) -> Result<()> {
    let expires_at = token.expires_in.map(|secs| {
        let now = chrono::Utc::now();
        let exp = now + chrono::Duration::seconds(secs as i64);
        exp.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    });
    conn.execute(
        "INSERT INTO oauth_tokens (id, access_token, refresh_token, expires_at, token_type, scope)
         VALUES (1, ?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
            access_token = excluded.access_token,
            refresh_token = excluded.refresh_token,
            expires_at = excluded.expires_at,
            token_type = excluded.token_type,
            scope = excluded.scope,
            obtained_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        params![
            token.access_token,
            token.refresh_token,
            expires_at,
            token.token_type,
            token.scope,
        ],
    )?;
    Ok(())
}

/// Read the stored access token. Returns `None` if no token is stored.
pub fn get_access_token(conn: &Connection) -> Result<Option<String>> {
    let v: Option<String> = conn
        .prepare("SELECT access_token FROM oauth_tokens WHERE id = 1")?
        .query_row([], |r| r.get::<_, String>(0))
        .ok();
    Ok(v)
}

/// Returns `true` if an OAuth token is stored (i.e., the user has connected).
pub fn has_token(conn: &Connection) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM oauth_tokens WHERE id = 1 AND access_token IS NOT NULL)",
        [],
        |r| r.get(0),
    )?;
    Ok(exists)
}

/// Delete the stored token (disconnect). Returns `true` if a row was deleted.
pub fn delete_token(conn: &Connection) -> Result<bool> {
    let rows = conn.execute("DELETE FROM oauth_tokens WHERE id = 1", [])?;
    Ok(rows > 0)
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
        conn
    }

    fn sample_token(access: &str) -> OAuthToken {
        OAuthToken {
            access_token: access.into(),
            refresh_token: Some("refresh456".into()),
            expires_in: Some(86400),
            token_type: "bearer".into(),
            scope: Some("conversations".into()),
        }
    }

    #[test]
    fn store_and_read_token() {
        let conn = fresh_db();
        assert!(!has_token(&conn).unwrap());

        store_token(&conn, &sample_token("abc123")).unwrap();

        assert!(has_token(&conn).unwrap());
        assert_eq!(get_access_token(&conn).unwrap(), Some("abc123".to_string()));
    }

    #[test]
    fn store_token_upserts() {
        let conn = fresh_db();
        store_token(&conn, &sample_token("token1")).unwrap();
        assert_eq!(get_access_token(&conn).unwrap(), Some("token1".to_string()));

        store_token(&conn, &sample_token("token2")).unwrap();
        assert_eq!(get_access_token(&conn).unwrap(), Some("token2".to_string()));
    }

    #[test]
    fn delete_token_removes_row() {
        let conn = fresh_db();
        store_token(&conn, &sample_token("abc")).unwrap();
        assert!(has_token(&conn).unwrap());

        let deleted = delete_token(&conn).unwrap();
        assert!(deleted);
        assert!(!has_token(&conn).unwrap());
    }

    #[test]
    fn delete_token_when_none_returns_false() {
        let conn = fresh_db();
        let deleted = delete_token(&conn).unwrap();
        assert!(!deleted);
    }

    #[test]
    fn has_token_returns_false_on_fresh_db() {
        let conn = fresh_db();
        assert!(!has_token(&conn).unwrap());
    }

    /// SY-07: the dead legacy path is GONE — `OAuthConfig`, `handle_callback`,
    /// `CallbackResult` and the placeholder `exchange_code` were removed with
    /// the Tauri-loopback receiver they served. This compile-level guard keeps
    /// them from quietly returning: the module must expose exactly the four
    /// token-storage helpers + the wire struct.
    #[test]
    fn sy07_legacy_loopback_path_stays_deleted() {
        // has_token still serves require_auth's legacy-DB fallback.
        let conn = fresh_db();
        assert!(!has_token(&conn).unwrap());
        // store/read/delete round-trip on the single-row store.
        store_token(&conn, &sample_token("live")).unwrap();
        assert_eq!(get_access_token(&conn).unwrap(), Some("live".into()));
        assert!(delete_token(&conn).unwrap());
    }
}
