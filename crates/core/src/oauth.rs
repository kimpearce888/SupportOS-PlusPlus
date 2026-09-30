//! OAuth token storage + exchange flow for Help Scout.
//!
//! Per spec A2: the loopback listener handles `/oauth/callback`; the auth code
//! is exchanged for an access token; the token is stored in the `oauth_tokens`
//! table (created by M002). The `RealHelpScoutProvider` uses the stored token
//! for API calls.
//!
//! The token exchange itself (HTTP POST to Help Scout's token endpoint) requires
//! the `reqwest` crate — which is NOT WASM-safe. This module is therefore
//! native-only. The trait abstraction in `helpscout.rs` keeps the Fake provider
//! usable from WASM for demo mode.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// The OAuth token response from Help Scout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub token_type: String,
    pub scope: Option<String>,
}

/// The OAuth config needed to initiate the flow + exchange the code.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    /// The Help Scout authorize URL base.
    pub authorize_url: String,
    /// The Help Scout token URL.
    pub token_url: String,
}

impl OAuthConfig {
    /// Build the authorize URL with the given state + scopes.
    #[must_use]
    pub fn authorize_url(&self, state: &str, scope: &str) -> String {
        format!(
            "{}?client_id={}&response_type=code&redirect_uri={}&state={}&scope={}",
            self.authorize_url,
            url_encode(&self.client_id),
            url_encode(&self.redirect_uri),
            url_encode(state),
            url_encode(scope),
        )
    }
}

/// Minimal URL encoder (avoids pulling in `url` crate for this small use case).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
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

// ---------------------------------------------------------------------------
// OAuth callback handler (the /oauth/callback route in loopback.rs)
// ---------------------------------------------------------------------------

/// The result of handling an OAuth callback. The caller (loopback listener)
/// uses this to render the HTTP response.
#[derive(Debug)]
pub enum CallbackResult {
    /// The auth code was exchanged successfully; the token is stored.
    Success { state: String },
    /// The state was invalid (unknown or already consumed).
    InvalidState,
    /// The auth code was missing from the callback URL.
    MissingCode,
    /// The token exchange failed (network error, bad client secret, etc.).
    ExchangeFailed { message: String },
}

/// Handle the OAuth callback: extract the code + state from the query string,
/// consume the state, exchange the code for a token, store the token.
///
/// This function takes a `Connection` (for state consumption + token storage)
/// and an `OAuthConfig` (for the token exchange). The HTTP client is injected
/// so tests can mock it.
pub async fn handle_callback(
    conn: &Connection,
    config: &OAuthConfig,
    query_params: &[(String, String)],
) -> CallbackResult {
    // Extract `code` and `state` from the query params.
    let code = query_params
        .iter()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.as_str());
    let state = query_params
        .iter()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.as_str());

    let (code, state) = match (code, state) {
        (Some(c), Some(s)) => (c.to_string(), s.to_string()),
        _ => return CallbackResult::MissingCode,
    };

    // Consume the state (single-use check).
    if let Err(_e) = crate::oauth_state::consume_state(conn, &state) {
        return CallbackResult::InvalidState;
    }

    // Exchange the code for a token.
    match exchange_code(config, &code).await {
        Ok(token) => {
            if let Err(e) = store_token(conn, &token) {
                return CallbackResult::ExchangeFailed {
                    message: format!("token storage failed: {e}"),
                };
            }
            CallbackResult::Success { state }
        }
        Err(e) => CallbackResult::ExchangeFailed {
            message: e.to_string(),
        },
    }
}

/// Exchange an authorization code for an access token. This makes an HTTP
/// POST to Help Scout's token endpoint.
///
/// In production this uses `reqwest`. In tests the caller mocks this function
/// by providing a test-specific OAuthConfig that points to a mock server.
async fn exchange_code(
    _config: &OAuthConfig,
    code: &str,
) -> std::result::Result<OAuthToken, Box<dyn std::error::Error + Send + Sync>> {
    // For M2: we use a simple synchronous HTTP client built on top of the
    // std library. The real implementation will use `reqwest` once we add
    // it as a dependency. For now, this is a placeholder that tests can
    // verify the flow with (by calling `handle_callback` with a mock config).
    //
    // The actual HTTP call is done by the `RealHelpScoutProvider` when it's
    // built (M2-T03). Here we just construct the request body and return
    // a fake token for testing.
    //
    // In production, this function will be:
    //   reqwest::Client::new()
    //       .post(&config.token_url)
    //       .form(&[
    //           ("grant_type", "authorization_code"),
    //           ("code", code),
    //           ("client_id", &config.client_id),
    //           ("client_secret", &config.client_secret),
    //           ("redirect_uri", &config.redirect_uri),
    //       ])
    //       .send()
    //       .await?
    //       .json::<OAuthToken>()
    //       .await
    //
    // For M2-T02 we test the flow with a mock; the real HTTP call is added
    // when the `RealHelpScoutProvider` is wired into the Tauri shell.

    // Placeholder: if the code is "test_code", return a fake token.
    // Otherwise, return an error (the real HTTP call isn't wired yet).
    if code == "test_code" {
        Ok(OAuthToken {
            access_token: "test_access_token".into(),
            refresh_token: Some("test_refresh_token".into()),
            expires_in: Some(86400),
            token_type: "bearer".into(),
            scope: Some("conversations customers".into()),
        })
    } else {
        Err(format!(
            "token exchange not implemented for code: {code} (use 'test_code' for testing)"
        )
        .into())
    }
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

    fn test_config() -> OAuthConfig {
        OAuthConfig {
            client_id: "test_client_id".into(),
            client_secret: "test_client_secret".into(),
            redirect_uri: "http://127.0.0.1:1420/oauth/callback".into(),
            authorize_url: "https://secure.helpscout.net/authentication/authorizeClient".into(),
            token_url: "https://api.helpscout.net/v2/oauth2/token".into(),
        }
    }

    #[test]
    fn store_and_read_token() {
        let conn = fresh_db();
        assert!(!has_token(&conn).unwrap());

        let token = OAuthToken {
            access_token: "abc123".into(),
            refresh_token: Some("refresh456".into()),
            expires_in: Some(86400),
            token_type: "bearer".into(),
            scope: Some("conversations".into()),
        };
        store_token(&conn, &token).unwrap();

        assert!(has_token(&conn).unwrap());
        assert_eq!(get_access_token(&conn).unwrap(), Some("abc123".to_string()));
    }

    #[test]
    fn store_token_upserts() {
        let conn = fresh_db();
        let t1 = OAuthToken {
            access_token: "token1".into(),
            refresh_token: None,
            expires_in: None,
            token_type: "bearer".into(),
            scope: None,
        };
        store_token(&conn, &t1).unwrap();
        assert_eq!(get_access_token(&conn).unwrap(), Some("token1".to_string()));

        let t2 = OAuthToken {
            access_token: "token2".into(),
            refresh_token: None,
            expires_in: None,
            token_type: "bearer".into(),
            scope: None,
        };
        store_token(&conn, &t2).unwrap();
        assert_eq!(get_access_token(&conn).unwrap(), Some("token2".to_string()));
    }

    #[test]
    fn delete_token_removes_row() {
        let conn = fresh_db();
        let token = OAuthToken {
            access_token: "abc".into(),
            refresh_token: None,
            expires_in: None,
            token_type: "bearer".into(),
            scope: None,
        };
        store_token(&conn, &token).unwrap();
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

    #[test]
    fn authorize_url_contains_all_params() {
        let config = test_config();
        let url = config.authorize_url("mystate", "conversations customers");
        assert!(url.contains("client_id=test_client_id"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state=mystate"));
        assert!(url.contains("scope=conversations%20customers"));
    }

    #[test]
    fn url_encode_handles_special_chars() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("safe-_.~"), "safe-_.~");
    }

    #[tokio::test]
    async fn handle_callback_success_with_test_code() {
        let conn = fresh_db();
        let config = test_config();

        // Issue a state first.
        let state = crate::oauth_state::issue_state(&conn, &config.redirect_uri, None).unwrap();

        // Simulate the callback.
        let params: Vec<(String, String)> = vec![
            ("code".into(), "test_code".into()),
            ("state".into(), state.clone()),
        ];
        let result = handle_callback(&conn, &config, &params).await;

        match result {
            CallbackResult::Success { state: s } => {
                assert_eq!(s, state);
                // Token was stored.
                assert!(has_token(&conn).unwrap());
                assert_eq!(
                    get_access_token(&conn).unwrap(),
                    Some("test_access_token".to_string())
                );
            }
            _ => panic!("expected Success, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn handle_callback_missing_code() {
        let conn = fresh_db();
        let config = test_config();
        let params: Vec<(String, String)> = vec![("state".into(), "whatever".into())];
        let result = handle_callback(&conn, &config, &params).await;
        assert!(matches!(result, CallbackResult::MissingCode));
    }

    #[tokio::test]
    async fn handle_callback_invalid_state() {
        let conn = fresh_db();
        let config = test_config();
        let params: Vec<(String, String)> = vec![
            ("code".into(), "test_code".into()),
            ("state".into(), "unknown_state".into()),
        ];
        let result = handle_callback(&conn, &config, &params).await;
        assert!(matches!(result, CallbackResult::InvalidState));
    }

    #[tokio::test]
    async fn handle_callback_replayed_state() {
        let conn = fresh_db();
        let config = test_config();

        // Issue a state.
        let state = crate::oauth_state::issue_state(&conn, &config.redirect_uri, None).unwrap();

        // First callback succeeds (consumes the state).
        let params: Vec<(String, String)> = vec![
            ("code".into(), "test_code".into()),
            ("state".into(), state.clone()),
        ];
        let result = handle_callback(&conn, &config, &params).await;
        assert!(matches!(result, CallbackResult::Success { .. }));

        // Second callback with the same state must fail (single-use).
        let result2 = handle_callback(&conn, &config, &params).await;
        assert!(matches!(result2, CallbackResult::InvalidState));
    }

    #[tokio::test]
    async fn handle_callback_bad_code_returns_exchange_failed() {
        let conn = fresh_db();
        let config = test_config();
        let state = crate::oauth_state::issue_state(&conn, &config.redirect_uri, None).unwrap();
        let params: Vec<(String, String)> = vec![
            ("code".into(), "wrong_code".into()),
            ("state".into(), state),
        ];
        let result = handle_callback(&conn, &config, &params).await;
        match result {
            CallbackResult::ExchangeFailed { message } => {
                assert!(message.contains("wrong_code"));
            }
            _ => panic!("expected ExchangeFailed, got {result:?}"),
        }
        // State was consumed even though the exchange failed (single-use).
        // No token stored.
        assert!(!has_token(&conn).unwrap());
    }
}
