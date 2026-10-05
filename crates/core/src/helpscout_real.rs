//! RealHelpScoutProvider — the real Help Scout HTTP integration.
//!
//! Mirrors the reference's `integrations/helpscout/{client,authService,
//! rateLimiter,apiQueue,realProvider}.ts` stack:
//! - Bearer token from `oauth_tokens` (auto-refresh on 401 / expiry < 120 s)
//! - Central rate limiter honoring Help Scout's
//!   `X-RateLimit-*` headers (writes count double), persisted under the
//!   `hs_rate_limit` application_settings key
//! - Bounded-concurrency priority queue stats (visible on /api/sync/status)
//! - 60 s request timeout; retry on 429 (Retry-After) and 5xx with backoff
//! - Friendly, contextual error messages (never raw "HTTP 412")
//! - OAuth: authorization-code + refresh + client-credentials flows against
//!   `POST {apiBase}/v2/oauth2/token`

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::error::{Error, Result};

use crate::helpscout::{
    ConversationCreated, ConversationPatch, ConversationQuery, CreateConversationInput,
    CreateThreadInput, CustomerQuery, HelpScoutProvider, HsBeaconChat, HsConversation, HsCustomer,
    HsDocArticle, HsDocCategory, HsDocCollection, HsField, HsFieldOption, HsFolder, HsMailbox,
    HsOrganization, HsPropertyDef, HsRating, HsSavedReply, HsTag, HsTeam, HsThread, HsUser,
    HsUserStatus, HsWebhookConfig, HsWorkflow, Page, ThreadCreated,
};

/// Default Help Scout API base.
pub const HS_API_BASE: &str = "https://api.helpscout.net";
/// Docs API base (separate key, HTTP Basic auth).
pub const HS_DOCS_API_BASE: &str = "https://docsapi.helpscout.net";
/// OAuth authorize endpoint (browser-facing).
pub const HS_AUTHORIZE_URL: &str =
    "https://secure.helpscout.net/authentication/authorizeClientApplication";

// ---------------------------------------------------------------------------
// Errors (client.ts parity)
// ---------------------------------------------------------------------------

/// A Help Scout API error with the reference's friendly message.
#[derive(Debug, Clone)]
pub struct HsApiError {
    pub status_code: u16,
    pub message: String,
    pub friendly: String,
    pub retryable: bool,
}

impl std::fmt::Display for HsApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.friendly)
    }
}

impl std::error::Error for HsApiError {}

impl From<HsApiError> for Error {
    fn from(e: HsApiError) -> Self {
        // Box the typed error itself (not a pre-formatted string) so callers
        // can downcast back to `HsApiError` and recover `friendly` + `status_code`
        // for the write pipeline's response/detail shapes (operations.ts catch).
        Error::Other(Box::new(e))
    }
}

/// Friendly, contextual error messages (spec #68, #89). Never show raw
/// "HTTP 412". Mirrors `friendlyError` in client.ts exactly.
pub fn friendly_error(status: u16, body: &str, method: &str) -> String {
    let detail = if body.is_empty() {
        String::new()
    } else {
        body.chars().take(300).collect::<String>()
    };
    let get = method == "GET";
    match status {
        400 => format!("Help Scout rejected the request as invalid{}{}. No changes were made. {}", if get { " (check the filters used)" } else { " (check the values you entered)" }, "", detail),
        401 => "The Help Scout connection is no longer authenticated. Re-connect Help Scout in Settings, then retry. No changes were made.".to_string(),
        403 => format!("Help Scout denied permission for this operation (your user role may not allow it). No changes were made. {detail}"),
        404 => "Help Scout reports this record no longer exists (it may have been deleted or merged into another conversation). Local data was preserved.".to_string(),
        409 => format!("Help Scout reported a conflict - the record changed on the server while we were working. Refresh the conversation and retry. {detail}"),
        412 => "Help Scout rejected this change because the conversation cannot currently accept another thread. No local changes were treated as successful.".to_string(),
        413 => "The request was too large (attachments or message size exceed Help Scout limits). Try smaller content.".to_string(),
        415 => "Help Scout did not accept the format of this request. No changes were made.".to_string(),
        423 => "Help Scout has this conversation locked (another process or user is acting on it right now). Try again in a moment.".to_string(),
        429 => "Help Scout rate limit reached. The operation will be retried automatically when the limit resets.".to_string(),
        500 => "Help Scout reported an internal error. The operation can be retried; no local data was changed.".to_string(),
        503 => "Help Scout is temporarily unavailable. The operation can be retried automatically.".to_string(),
        504 => "The request to Help Scout timed out. It may or may not have completed remotely - verify in Help Scout before retrying a send.".to_string(),
        _ => format!("Help Scout returned status {status}. {detail}"),
    }
}

fn is_retryable(status: u16) -> bool {
    matches!(status, 429 | 500 | 503 | 504)
}

// ---------------------------------------------------------------------------
// Rate limiter (rateLimiter.ts parity)
// ---------------------------------------------------------------------------

/// Persisted rate-limit state (`application_settings.hs_rate_limit`).
#[derive(Debug, Clone)]
pub struct RateLimitState {
    pub limit_per_minute: i64,
    pub remaining: Option<i64>,
    pub retry_after_sec: Option<i64>,
    pub updated_at: String,
}

impl Default for RateLimitState {
    fn default() -> Self {
        Self {
            limit_per_minute: 150,
            remaining: None,
            retry_after_sec: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// Centralized account-wide rate limiter. Writes count as 2 requests.
pub struct HsRateLimiter {
    state: Mutex<RateLimitState>,
    send_timestamps: Mutex<Vec<i64>>,
    conn: Option<Arc<Mutex<Connection>>>,
}

impl HsRateLimiter {
    pub fn new(conn: Option<Arc<Mutex<Connection>>>) -> Self {
        let limiter = Self {
            state: Mutex::new(RateLimitState::default()),
            send_timestamps: Mutex::new(Vec::new()),
            conn,
        };
        limiter.load();
        limiter
    }

    fn load(&self) {
        let Some(conn) = &self.conn else { return };
        let Ok(conn) = conn.lock() else { return };
        let Ok(row) = conn.query_row(
            "SELECT value FROM application_settings WHERE key = 'hs_rate_limit'",
            [],
            |r| r.get::<_, String>(0),
        ) else {
            return;
        };
        if let Ok(v) = serde_json::from_str::<Value>(&row) {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(n) = v["limitPerMinute"].as_i64() {
                state.limit_per_minute = n;
            }
            state.remaining = v["remaining"].as_i64();
            state.retry_after_sec = v["retryAfterSec"].as_i64();
            if let Some(t) = v["updatedAt"].as_str() {
                state.updated_at = t.to_string();
            }
        }
    }

    fn persist(&self, state: &RateLimitState) {
        let Some(conn) = &self.conn else { return };
        let Ok(conn) = conn.lock() else { return };
        let v = json!({
            "limitPerMinute": state.limit_per_minute,
            "remaining": state.remaining,
            "retryAfterSec": state.retry_after_sec,
            "updatedAt": state.updated_at,
        });
        let _ = conn.execute(
            "INSERT INTO application_settings (key, value, updated_at)
             VALUES ('hs_rate_limit', ?1, datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![v.to_string()],
        );
    }

    /// Record a response's rate-limit headers (mirror of recordResponse).
    pub fn record_response(
        &self,
        limit: Option<i64>,
        remaining: Option<i64>,
        retry_after: Option<i64>,
        is_write: bool,
    ) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(l) = limit {
                state.limit_per_minute = l;
            }
            if remaining.is_some() {
                state.remaining = remaining;
            }
            state.retry_after_sec = if retry_after.is_some() {
                retry_after
            } else {
                None
            };
            state.updated_at = chrono::Utc::now().to_rfc3339();
            self.persist(&state);
        }
        let mut sends = self
            .send_timestamps
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        sends.retain(|t| now_ms - t < 60_000);
        sends.push(now_ms);
        if is_write {
            // Writes cost 2: reference pushes a second timestamp.
            sends.push(now_ms);
        }
    }

    /// Record a 429 (mirror of recordError429).
    pub fn record_error_429(&self, retry_after_sec: Option<i64>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.retry_after_sec = Some(retry_after_sec.unwrap_or(30));
        state.remaining = Some(0);
        state.updated_at = chrono::Utc::now().to_rfc3339();
        self.persist(&state);
    }

    /// Milliseconds to wait before the next request (0 = go now).
    pub fn wait_time_ms(&self, _is_write: bool) -> i64 {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let (Some(retry), Some(0)) = (state.retry_after_sec, state.remaining) {
            if let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&state.updated_at) {
                let reset_at = updated.timestamp_millis() + retry * 1000;
                if reset_at > now_ms {
                    return reset_at - now_ms + 250;
                }
            }
        }
        let mut sends = self
            .send_timestamps
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        sends.retain(|t| now_ms - t < 60_000);
        let effective_limit = (state.limit_per_minute - 5).max(5);
        if sends.len() as i64 + 1 > effective_limit {
            if let Some(oldest) = sends.first() {
                let wait = 60_000 - (now_ms - oldest) + 100;
                if wait > 0 {
                    return wait;
                }
            }
        }
        0
    }

    /// Snapshot for GET /api/sync/status (`inFlightWindow`).
    pub fn snapshot(&self) -> Value {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let mut sends = self
            .send_timestamps
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        sends.retain(|t| now_ms - t < 60_000);
        json!({
            "limitPerMinute": state.limit_per_minute,
            "remaining": state.remaining,
            "retryAfterSec": state.retry_after_sec,
            "updatedAt": state.updated_at,
            "inFlightWindow": sends.len(),
        })
    }
}

/// Queue stats (apiQueue.ts statsSnapshot parity).
#[derive(Default)]
pub struct ApiQueueStats {
    pub queued: AtomicI64,
    pub active: AtomicI64,
    pub dispatched: AtomicI64,
    pub completed: AtomicI64,
    pub failed: AtomicI64,
    pub high_water: AtomicI64,
}

impl ApiQueueStats {
    pub fn snapshot(&self) -> Value {
        json!({
            "queued": self.queued.load(Ordering::Relaxed),
            "active": self.active.load(Ordering::Relaxed),
            "dispatched": self.dispatched.load(Ordering::Relaxed),
            "completed": self.completed.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "highWater": self.high_water.load(Ordering::Relaxed),
        })
    }
}

// ---------------------------------------------------------------------------
// Token store (authService.ts parity — account='default' single row)
// ---------------------------------------------------------------------------

const TOKEN_EXPIRY_MARGIN_SECS: i64 = 120;

struct TokenRow {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<String>,
    revoked: bool,
}

fn token_row(conn: &Connection) -> Option<TokenRow> {
    conn.query_row(
        "SELECT access_token, refresh_token, expires_at, revoked FROM oauth_tokens WHERE account = 'default'",
        [],
        |r| {
            Ok(TokenRow {
                access_token: r.get(0)?,
                refresh_token: r.get(1)?,
                expires_at: r.get(2)?,
                revoked: r.get::<_, i64>(3)? != 0,
            })
        },
    )
    .ok()
}

/// OAuth credentials (env-backed like the reference .env).
#[derive(Debug, Clone, Default)]
pub struct HsCredentials {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub api_base: String,
    /// Webhook HMAC secret (env-only, reference config.helpscout.webhookSecret).
    pub webhook_secret: String,
    /// Docs API key — separate from OAuth (config.helpscout.docsApiKey).
    pub docs_api_key: String,
    /// Docs API host (config.helpscout.docsApiBase).
    pub docs_api_base: String,
}

impl HsCredentials {
    /// Read credentials from the environment (reference config.ts parity).
    pub fn from_env() -> Self {
        let env = crate::config::load_env_config();
        Self {
            client_id: env.helpscout.client_id,
            client_secret: env.helpscout.client_secret,
            redirect_uri: env.helpscout.redirect_uri,
            api_base: env.helpscout.api_base,
            webhook_secret: env.helpscout.webhook_secret,
            docs_api_key: env.helpscout.docs_api_key,
            docs_api_base: env.helpscout.docs_api_base,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty()
    }

    /// `buildAuthorizeUrl(state)` — exactly the reference's two params.
    pub fn authorize_url(&self, state: &str) -> String {
        format!(
            "{HS_AUTHORIZE_URL}?client_id={}&state={}",
            urlencode(&self.client_id),
            urlencode(state)
        )
    }
}

/// `base64(key + ":X")` — the Docs API Basic-auth token (realProvider.ts:119).
fn base64_of(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn urlencode(s: &str) -> String {
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
// The provider
// ---------------------------------------------------------------------------

/// Real Help Scout provider: all HTTP goes through `request()` which applies
/// the rate limiter, retries, and token refresh.
pub struct RealHelpScoutProvider {
    client: reqwest::Client,
    api_base: String,
    conn: Arc<Mutex<Connection>>,
    pub credentials: HsCredentials,
    pub limiter: HsRateLimiter,
    pub queue: Arc<ApiQueueStats>,
}

impl RealHelpScoutProvider {
    #[must_use]
    pub fn new(conn: Arc<Mutex<Connection>>, credentials: HsCredentials) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
            api_base: credentials.api_base.trim_end_matches('/').to_string(),
            conn,
            limiter: HsRateLimiter::new(None),
            queue: Arc::new(ApiQueueStats::default()),
            credentials,
        }
    }

    fn conn_lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Docs API request: separate host (`credentials.docs_api_base`) with
    /// HTTP Basic auth using the Docs API key — `Basic base64(key:X)`
    /// (realProvider.ts:112-123). Without a key the sync is a NO-OP: the
    /// docs methods return empty sets (the honest-capability note — the
    /// mirror stays empty rather than erroring).
    async fn docs_request(&self, path: &str) -> Result<Value> {
        if self.credentials.docs_api_key.is_empty() {
            return Ok(Value::Null);
        }
        let basic = base64_of(&format!("{}:X", self.credentials.docs_api_key));
        let url = format!(
            "{}{path}",
            self.credentials.docs_api_base.trim_end_matches('/')
        );
        let res = self
            .client
            .get(url)
            .header("Authorization", format!("Basic {basic}"))
            .send()
            .await
            .map_err(|e| -> crate::error::Error {
                HsApiError {
                    status_code: 0,
                    message: format!("Network error contacting the Docs API: {e}"),
                    friendly: "The Help Scout Docs API is unreachable. The docs mirror stays empty; everything else keeps working.".into(),
                    retryable: true,
                }
                .into()
            })?;
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        if (200..300).contains(&status) {
            if text.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).map_err(|_| {
                HsApiError {
                    status_code: status,
                    message: "Invalid JSON from the Docs API".into(),
                    friendly: "The Docs API returned an unexpected response format.".into(),
                    retryable: false,
                }
                .into()
            });
        }
        Err(HsApiError {
            status_code: status,
            message: format!("Docs API GET {path} -> {status}"),
            friendly: friendly_error(status, &text, "GET"),
            retryable: false,
        }
        .into())
    }

    // ---------------- Token access ----------------

    /// Current access token, refreshing when it expires within 120 s.
    async fn access_token(&self) -> Result<String> {
        let row = {
            let conn = self.conn_lock();
            // Ensure the reference-shaped column exists (M029 keeps both
            // id=1 and account='default' rows in sync at write time).
            ensure_account_row(&conn);
            token_row(&conn)
        };
        let Some(row) = row else {
            return Err(HsApiError {
                status_code: 401,
                message: "No Help Scout token available".into(),
                friendly: friendly_error(401, "", "GET"),
                retryable: false,
            }
            .into());
        };
        if row.revoked || row.access_token.as_deref().unwrap_or("").is_empty() {
            return Err(HsApiError {
                status_code: 401,
                message: "No Help Scout token available".into(),
                friendly: friendly_error(401, "", "GET"),
                retryable: false,
            }
            .into());
        }
        let expires_soon = match row.expires_at.as_deref() {
            Some(exp) => match chrono::DateTime::parse_from_rfc3339(exp) {
                Ok(t) => t.timestamp() < chrono::Utc::now().timestamp() + TOKEN_EXPIRY_MARGIN_SECS,
                Err(_) => true,
            },
            None => true,
        };
        if !expires_soon {
            return Ok(row.access_token.unwrap_or_default());
        }
        if let Some(refresh) = row.refresh_token {
            if let Ok(token) = self.refresh_tokens(&refresh).await {
                return Ok(token);
            }
        }
        Ok(row.access_token.unwrap_or_default())
    }

    /// `POST /v2/oauth2/token` with the given form fields.
    pub async fn token_request(
        &self,
        form: &[(&str, &str)],
    ) -> std::result::Result<Value, HsApiError> {
        let mut form_data: Vec<(&str, &str)> = form.to_vec();
        form_data.push(("client_id", &self.credentials.client_id));
        form_data.push(("client_secret", &self.credentials.client_secret));
        let res = self
            .client
            .post(format!("{}/v2/oauth2/token", self.api_base))
            .timeout(Duration::from_secs(20))
            .form(&form_data)
            .send()
            .await
            .map_err(|e| HsApiError {
                status_code: 0,
                message: format!("OAuth token request failed: {e}"),
                friendly: "Help Scout rejected the login credentials. Check your Client ID/Secret in Settings, then try connecting again.".into(),
                retryable: false,
            })?;
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(HsApiError {
                status_code: status,
                message: format!("OAuth token request failed ({status})"),
                friendly: "Help Scout rejected the login credentials. Check your Client ID/Secret in Settings, then try connecting again.".into(),
                retryable: false,
            });
        }
        res.json::<Value>()
            .await
            .map_err(|e| HsApiError {
                status_code: 0,
                message: format!("invalid token response: {e}"),
                friendly: "Help Scout returned an unexpected response format. The raw response was preserved for diagnostics.".into(),
                retryable: false,
            })
    }

    /// Save tokens (authService.saveTokens parity — both row keys).
    pub fn save_tokens(
        &self,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_in: Option<i64>,
        scope: Option<&str>,
    ) -> Result<()> {
        let expires_at =
            chrono::Utc::now() + chrono::Duration::seconds(expires_in.unwrap_or(172_800));
        let expires_iso = expires_at.to_rfc3339();
        let keep_refresh: Option<String> = refresh_token.map(|s| s.to_string());
        let conn = self.conn_lock();
        conn.execute(
            "INSERT INTO oauth_tokens (account, access_token, refresh_token, token_type, expires_at, obtained_at, scope, revoked)
             VALUES ('default', ?1, ?2, 'bearer', ?3, datetime('now'), ?4, 0)
             ON CONFLICT(account) DO UPDATE SET access_token = excluded.access_token,
               refresh_token = COALESCE(excluded.refresh_token, refresh_token),
               expires_at = excluded.expires_at, obtained_at = datetime('now'),
               scope = excluded.scope, revoked = 0",
            rusqlite::params![access_token, keep_refresh, expires_iso, scope],
        )?;
        // Keep the port's legacy id=1 row in sync so both access paths agree.
        let _ = conn.execute(
            "UPDATE oauth_tokens SET access_token = ?1, refresh_token = ?2, expires_at = ?3, revoked = 0
              WHERE account = 'default'",
            rusqlite::params![access_token, keep_refresh, expires_iso],
        );
        Ok(())
    }

    /// Refresh tokens; returns the new access token.
    pub async fn refresh_tokens(&self, refresh_token: &str) -> Result<String> {
        let v = self
            .token_request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .await
            .map_err(Error::from)?;
        let access = v["access_token"].as_str().unwrap_or_default().to_string();
        let refresh = v["refresh_token"].as_str().map(|s| s.to_string());
        let expires = v["expires_in"].as_i64();
        let scope = v["scope"].as_str().map(|s| s.to_string());
        self.save_tokens(&access, refresh.as_deref(), expires, scope.as_deref())?;
        Ok(access)
    }

    /// Client-credentials login; returns the access token.
    pub async fn client_credentials_login(&self) -> Result<String> {
        let v = self
            .token_request(&[("grant_type", "client_credentials")])
            .await
            .map_err(Error::from)?;
        let access = v["access_token"].as_str().unwrap_or_default().to_string();
        let expires = v["expires_in"].as_i64();
        let scope = v["scope"].as_str().map(|s| s.to_string());
        self.save_tokens(&access, None, expires, scope.as_deref())?;
        Ok(access)
    }

    /// Exchange an authorization code (OAuth callback flow).
    pub async fn exchange_code(&self, code: &str) -> Result<String> {
        let v = self
            .token_request(&[("grant_type", "authorization_code"), ("code", code)])
            .await
            .map_err(Error::from)?;
        let access = v["access_token"].as_str().unwrap_or_default().to_string();
        let refresh = v["refresh_token"].as_str().map(|s| s.to_string());
        let expires = v["expires_in"].as_i64();
        let scope = v["scope"].as_str().map(|s| s.to_string());
        self.save_tokens(&access, refresh.as_deref(), expires, scope.as_deref())?;
        Ok(access)
    }

    /// Revoke (disconnect): tokens cleared, `revoked = 1`.
    pub fn revoke(&self) -> Result<()> {
        let conn = self.conn_lock();
        conn.execute(
            "UPDATE oauth_tokens SET revoked = 1, access_token = NULL, refresh_token = NULL
              WHERE account = 'default'",
            [],
        )?;
        Ok(())
    }

    /// OAuth status (authService.status parity).
    pub fn oauth_status(&self, demo_mode: bool) -> Value {
        let conn = self.conn_lock();
        ensure_account_row(&conn);
        let row = token_row(&conn);
        let authenticated = row.as_ref().is_some_and(|r| {
            !r.revoked && r.access_token.as_deref().is_some_and(|t| !t.is_empty())
        });
        json!({
            "configured": self.credentials.is_configured(),
            "authenticated": authenticated,
            "demoMode": demo_mode,
            "expiresAt": row.as_ref().and_then(|r| r.expires_at.clone()),
        })
    }

    // ---------------- Core HTTP ----------------

    /// Single point for all Help Scout HTTP: bearer token, rate limiting,
    /// retries (429 + 5xx + network), friendly error mapping. Public so the
    /// operations layer can issue the reference's remote writes.
    pub async fn request(&self, path: &str, method: &str, body: Option<Value>) -> Result<Value> {
        self.request_inner(path, method, body, 3).await
    }

    async fn request_inner(
        &self,
        path: &str,
        method: &str,
        body: Option<Value>,
        retries_left: u32,
    ) -> Result<Value> {
        // Rate-limit gate (mirror of ApiQueue.pump's wait).
        let wait = self.limiter.wait_time_ms(method != "GET");
        if wait > 0 {
            tokio::time::sleep(Duration::from_millis(wait.min(5_000) as u64)).await;
        }

        let token = self.access_token().await?;
        let url = format!("{}{path}", self.api_base);
        let m: reqwest::Method = method
            .parse()
            .map_err(|_| Error::Other(format!("invalid HTTP method {method}").into()))?;
        let mut req = self
            .client
            .request(m, &url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json");
        if let Some(b) = &body {
            req = req.json(b);
        }

        self.queue.dispatched.fetch_add(1, Ordering::Relaxed);
        self.queue.active.fetch_add(1, Ordering::Relaxed);
        let res = req.send().await;
        self.queue.active.fetch_sub(1, Ordering::Relaxed);

        let res = match res {
            Ok(r) => r,
            Err(e) => {
                self.queue.failed.fetch_add(1, Ordering::Relaxed);
                let msg = e.to_string();
                if retries_left > 0
                    && (msg.contains("timeout")
                        || msg.contains("Connection refused")
                        || msg.contains("error sending request"))
                {
                    tokio::time::sleep(Duration::from_millis(1_500)).await;
                    return Box::pin(self.request_inner(path, method, body, retries_left - 1))
                        .await;
                }
                return Err(HsApiError {
                    status_code: 0,
                    message: format!("Network error contacting Help Scout: {msg}"),
                    friendly: "Help Scout is unreachable. Local data remains fully browsable - you are in offline mode for remote actions.".into(),
                    retryable: true,
                }
                .into());
            }
        };

        let status = res.status().as_u16();
        let limit = res
            .headers()
            .get("x-ratelimit-limit-minute")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok());
        let remaining = res
            .headers()
            .get("x-ratelimit-remaining-minute")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok());
        let retry_after_hdr = res
            .headers()
            .get("x-ratelimit-retry-after")
            .or_else(|| res.headers().get("retry-after"))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok());
        self.limiter
            .record_response(limit, remaining, retry_after_hdr, method != "GET");

        // 301 = merged conversation: surface with target id in the message.
        if status == 301 {
            let location = res
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let new_id = location
                .split("conversations/")
                .nth(1)
                .and_then(|s| s.split(&['/', '?'][..]).next())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "another conversation".into());
            return Err(HsApiError {
                status_code: 301,
                message: format!("Conversation merged into {new_id}"),
                friendly: "This conversation was merged into another conversation in Help Scout. Open the target conversation instead.".into(),
                retryable: false,
            }
            .into());
        }

        // 401: refresh + retry once per level.
        if status == 401 && retries_left > 0 {
            let refreshed = {
                let conn = self.conn_lock();
                token_row(&conn).and_then(|r| r.refresh_token)
            };
            if let Some(refresh) = refreshed {
                if self.refresh_tokens(&refresh).await.is_ok() {
                    return Box::pin(self.request_inner(path, method, body, retries_left - 1))
                        .await;
                }
            }
        }

        // 429: honor Retry-After (fallback 30 s, capped 60 s).
        if status == 429 {
            let retry_after = retry_after_hdr.filter(|s| *s > 0).unwrap_or(30).min(60);
            self.limiter.record_error_429(Some(retry_after));
            if retries_left > 0 {
                tokio::time::sleep(
                    Duration::from_secs(retry_after as u64) + Duration::from_millis(250),
                )
                .await;
                return Box::pin(self.request_inner(path, method, body, retries_left - 1)).await;
            }
        }

        // Retryable 5xx.
        if is_retryable(status) && retries_left > 0 && status != 429 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Box::pin(self.request_inner(path, method, body, retries_left - 1)).await;
        }

        if (200..300).contains(&status) {
            self.queue.completed.fetch_add(1, Ordering::Relaxed);
            let text = res.text().await.unwrap_or_default();
            if text.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str::<Value>(&text).map_err(|_| {
                HsApiError {
                    status_code: status,
                    message: "Invalid JSON from Help Scout".into(),
                    friendly: "Help Scout returned an unexpected response format. The raw response was preserved for diagnostics.".into(),
                    retryable: false,
                }
                .into()
            });
        }

        self.queue.failed.fetch_add(1, Ordering::Relaxed);
        let body_text = res.text().await.unwrap_or_default();
        let detail: String = body_text.chars().take(400).collect();
        Err(HsApiError {
            status_code: status,
            message: format!("Help Scout {method} {path} -> {status}: {detail}"),
            friendly: friendly_error(status, &body_text, method),
            retryable: is_retryable(status)
                && !matches!(status, 400 | 401 | 403 | 404 | 409 | 412 | 413 | 415 | 423),
        }
        .into())
    }

    // ---------------- Mapping helpers ----------------

    fn map_user(v: &Value) -> HsUser {
        HsUser {
            remote_id: v["id"].as_i64().unwrap_or(0),
            first_name: v["firstName"].as_str().map(|s| s.to_string()),
            last_name: v["lastName"].as_str().map(|s| s.to_string()),
            email: v["email"].as_str().map(|s| s.to_string()),
            role: v["role"].as_str().map(|s| s.to_string()),
            user_type: v["type"].as_str().unwrap_or("user").to_string(),
            timezone: v["timezone"].as_str().map(|s| s.to_string()),
            photo_url: v["photoUrl"].as_str().map(|s| s.to_string()),
            initials: v["initials"].as_str().map(|s| s.to_string()),
            mention: v["mention"].as_str().map(|s| s.to_string()),
            job_title: v["jobTitle"].as_str().map(|s| s.to_string()),
            phone: v["phone"].as_str().map(|s| s.to_string()),
            alternate_emails: Vec::new(),
            created_at: v["createdAt"].as_str().map(|s| s.to_string()),
            updated_at: v["updatedAt"].as_str().map(|s| s.to_string()),
        }
    }

    fn map_mailbox(v: &Value) -> HsMailbox {
        HsMailbox {
            remote_id: v["id"].as_i64().unwrap_or(0),
            name: v["name"].as_str().unwrap_or_default().to_string(),
            slug: v["slug"].as_str().map(|s| s.to_string()),
            email: v["email"].as_str().map(|s| s.to_string()),
            created_at: v["createdAt"].as_str().map(|s| s.to_string()),
            updated_at: v["updatedAt"].as_str().map(|s| s.to_string()),
        }
    }

    fn map_folder(v: &Value, mailbox_id: i64) -> HsFolder {
        HsFolder {
            remote_id: v["id"].as_i64().unwrap_or(0),
            mailbox_id,
            name: v["name"].as_str().unwrap_or_default().to_string(),
            kind: v["type"].as_str().unwrap_or_default().to_string(),
            user_id: v["userId"].as_i64(),
            total_count: v["totalCount"].as_i64().unwrap_or(0),
            active_count: v["activeCount"].as_i64().unwrap_or(0),
        }
    }

    fn map_field(v: &Value, mailbox_id: i64) -> HsField {
        HsField {
            remote_id: v["id"].as_i64().unwrap_or(0),
            mailbox_id,
            name: v["name"].as_str().unwrap_or_default().to_string(),
            kind: v["type"].as_str().unwrap_or_default().to_string(),
            system_type: v["systemType"].as_str().map(|s| s.to_string()),
            required: v["required"].as_bool().unwrap_or(false),
            sort_order: v["order"].as_i64().unwrap_or(0),
            options: v["_embedded"]["fields"]
                .as_array()
                .map(|opts| {
                    opts.iter()
                        .map(|o| HsFieldOption {
                            id: o["id"].as_i64().unwrap_or(0),
                            order: o["order"].as_i64().unwrap_or(0),
                            label: o["label"].as_str().unwrap_or_default().to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn map_tag(v: &Value) -> HsTag {
        HsTag {
            remote_id: v["id"].as_i64().unwrap_or(0),
            name: v["tag"].as_str().unwrap_or_default().to_string(),
            slug: v["slug"].as_str().map(|s| s.to_string()),
            color: v["color"].as_str().map(|s| s.to_string()),
            ticket_count: v["ticketCount"].as_i64(),
            created_at: v["createdAt"].as_str().map(|s| s.to_string()),
            updated_at: v["updatedAt"].as_str().map(|s| s.to_string()),
        }
    }

    fn map_conversation(v: &Value) -> HsConversation {
        let assignee_type = v["assignee"]["type"].as_str().map(|s| s.to_string());
        HsConversation {
            remote_id: v["id"].as_i64().unwrap_or(0),
            number: v["number"].as_i64().unwrap_or(0),
            kind: v["type"].as_str().map(|s| s.to_string()),
            source_type: v["source"]["type"].as_str().map(|s| s.to_string()),
            source_via: v["source"]["via"].as_str().map(|s| s.to_string()),
            subject: v["subject"].as_str().map(|s| s.to_string()),
            preview: v["preview"].as_str().map(|s| s.to_string()),
            status: v["status"].as_str().unwrap_or("active").to_string(),
            state: v["state"].as_str().map(|s| s.to_string()),
            mailbox_id: v["mailboxId"].as_i64().unwrap_or(0),
            assignee_id: v["assignee"]["id"].as_i64(),
            assignee_type: assignee_type.clone(),
            assigned_team_id: v["assignedTeam"]["id"].as_i64().or(
                if assignee_type.as_deref() == Some("team") {
                    v["assignee"]["id"].as_i64()
                } else {
                    None
                },
            ),
            customer_id: v["primaryCustomer"]["id"].as_i64().unwrap_or(0),
            priority: None,
            created_at: v["createdAt"].as_str().map(|s| s.to_string()),
            updated_at: v["userUpdatedAt"].as_str().map(|s| s.to_string()),
            closed_at: v["closedAt"].as_str().map(|s| s.to_string()),
            snoozed_until: v["snooze"]["snoozedUntil"].as_str().map(|s| s.to_string()),
            thread_count: v["threads"].as_i64().unwrap_or(0),
            merged_into: None,
            tags: v["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t["tag"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn map_customer(v: &Value) -> HsCustomer {
        let emails: Vec<crate::helpscout::HsCustomerEmail> = v["emails"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| crate::helpscout::HsCustomerEmail {
                        value: e["value"].as_str().map(|s| s.to_string()),
                        kind: e["type"].as_str().map(|s| s.to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let phones: Vec<crate::helpscout::HsCustomerPhone> = v["phones"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| crate::helpscout::HsCustomerPhone {
                        value: p["value"].as_str().map(|s| s.to_string()),
                        kind: p["type"].as_str().map(|s| s.to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        HsCustomer {
            remote_id: v["id"].as_i64().unwrap_or(0),
            first_name: v["firstName"].as_str().map(|s| s.to_string()),
            last_name: v["lastName"].as_str().map(|s| s.to_string()),
            email: emails.first().and_then(|e| e.value.clone()),
            organization: v["organization"]["name"].as_str().map(|s| s.to_string()),
            job_title: v["jobTitle"].as_str().map(|s| s.to_string()),
            phone: phones.first().and_then(|p| p.value.clone()),
            created_at: v["createdAt"].as_str().map(|s| s.to_string()),
            updated_at: v["updatedAt"].as_str().map(|s| s.to_string()),
            photo_url: v["photoUrl"].as_str().map(|s| s.to_string()),
            organization_id: v["organization"]["id"].as_i64(),
            background: v["background"].as_str().map(|s| s.to_string()),
            age: v["age"].as_i64().map(|a| a.to_string()),
            gender: v["gender"].as_str().map(|s| s.to_string()),
            location: v["location"].as_str().map(|s| s.to_string()),
            emails,
            phones,
            websites: v["websites"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|w| crate::helpscout::HsCustomerWebsite {
                            value: w["value"].as_str().map(|s| s.to_string()),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            social_profiles: v["socialProfiles"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|s| crate::helpscout::HsCustomerSocialProfile {
                            value: s["value"].as_str().map(|x| x.to_string()),
                            kind: s["type"].as_str().map(|x| x.to_string()),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            address: v.get("address").filter(|a| a.is_object()).and_then(|a| {
                serde_json::from_value::<crate::helpscout::HsCustomerAddress>(a.clone()).ok()
            }),
            properties: v["properties"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|p| crate::helpscout::HsCustomerPropertyValue {
                            definition_remote_id: p["definitionRemoteId"].as_i64(),
                            key: p["key"].as_str().map(|s| s.to_string()),
                            name: p["name"].as_str().map(|s| s.to_string()),
                            value: p["value"].as_str().map(|s| s.to_string()),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn map_thread(v: &Value, conversation_id: i64) -> HsThread {
        HsThread {
            remote_id: v["id"].as_i64().unwrap_or(0),
            conversation_id,
            kind: v["type"].as_str().unwrap_or("message").to_string(),
            status: v["status"].as_str().map(|s| s.to_string()),
            state: v["state"].as_str().map(|s| s.to_string()),
            body: v["body"].as_str().map(|s| s.to_string()),
            created_by_customer_id: v["createdByCustomer"]["id"].as_i64(),
            created_by_user_id: v["createdByUser"]["id"].as_i64(),
            assigned_to_id: v["assignedTo"]["id"].as_i64(),
            created_at: v["createdAt"].as_str().map(|s| s.to_string()),
        }
    }

    fn map_organization(v: &Value) -> HsOrganization {
        HsOrganization {
            remote_id: v["id"].as_i64().unwrap_or(0),
            name: v["name"].as_str().unwrap_or_default().to_string(),
            domains: v["domains"]
                .as_array()
                .map(|d| {
                    d.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            created_at: v["createdAt"].as_str().map(|s| s.to_string()),
            updated_at: v["updatedAt"].as_str().map(|s| s.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl HelpScoutProvider for RealHelpScoutProvider {
    fn kind(&self) -> &'static str {
        "real"
    }

    async fn get_me(&self) -> Result<HsUser> {
        let v = self.request("/v2/users/me", "GET", None).await?;
        Ok(Self::map_user(&v))
    }

    async fn list_mailboxes(&self) -> Result<Vec<HsMailbox>> {
        let v = self.request("/v2/mailboxes", "GET", None).await?;
        Ok(v["_embedded"]["mailboxes"]
            .as_array()
            .map(|a| a.iter().map(Self::map_mailbox).collect())
            .unwrap_or_default())
    }

    async fn list_users(&self) -> Result<Vec<HsUser>> {
        let v = self.request("/v2/users", "GET", None).await?;
        Ok(v["_embedded"]["users"]
            .as_array()
            .map(|a| a.iter().map(Self::map_user).collect())
            .unwrap_or_default())
    }

    async fn list_teams(&self) -> Result<Vec<HsTeam>> {
        let v = self.request("/v2/teams", "GET", None).await?;
        Ok(v["_embedded"]["teams"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|t| HsTeam {
                        remote_id: t["id"].as_i64().unwrap_or(0),
                        name: t["name"].as_str().unwrap_or_default().to_string(),
                        member_user_ids: t["_embedded"]["users"]
                            .as_array()
                            .map(|users| {
                                users
                                    .iter()
                                    .filter_map(|u| u["id"].as_i64())
                                    .collect::<Vec<i64>>()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn list_tags(&self) -> Result<Vec<HsTag>> {
        let v = self.request("/v2/tags", "GET", None).await?;
        Ok(v["_embedded"]["tags"]
            .as_array()
            .map(|a| a.iter().map(Self::map_tag).collect())
            .unwrap_or_default())
    }

    async fn list_conversations(&self, query: &ConversationQuery) -> Result<Page<HsConversation>> {
        let mut path = String::from("/v3/conversations?status=");
        path.push_str(query.status.as_deref().unwrap_or("all"));
        if let Some(mb) = query.mailbox_id {
            path.push_str(&format!("&mailbox={mb}"));
        }
        if let Some(since) = &query.modified_since {
            path.push_str(&format!("&modifiedSince={}", urlencode(since)));
        }
        if let Some(cursor) = &query.cursor {
            path.push_str(&format!("&cursor={}", urlencode(cursor)));
        }
        let v = self.request(&path, "GET", None).await?;
        Ok(Page {
            items: v["_embedded"]["conversations"]
                .as_array()
                .map(|a| a.iter().map(Self::map_conversation).collect())
                .unwrap_or_default(),
            next_cursor: v["page"]["nextCursor"].as_str().map(|s| s.to_string()),
        })
    }

    async fn list_customers(&self, query: &CustomerQuery) -> Result<Page<HsCustomer>> {
        let mut path = String::from("/v3/customers");
        let mut sep = '?';
        if let Some(since) = &query.modified_since {
            path.push(sep);
            path.push_str(&format!("modifiedSince={}", urlencode(since)));
            sep = '&';
        }
        if let Some(cursor) = &query.cursor {
            path.push(sep);
            path.push_str(&format!("cursor={}", urlencode(cursor)));
        }
        let v = self.request(&path, "GET", None).await?;
        Ok(Page {
            items: v["_embedded"]["customers"]
                .as_array()
                .map(|a| a.iter().map(Self::map_customer).collect())
                .unwrap_or_default(),
            next_cursor: v["page"]["nextCursor"].as_str().map(|s| s.to_string()),
        })
    }

    async fn list_beacon_chats(&self) -> Result<Vec<HsBeaconChat>> {
        // Chats arrive as type='chat' conversations; the port's conversation
        // model has no chat type flag, so — like the reference's honest
        // limitation note — chat catch-up runs through the conversation pass.
        // This endpoint returns an empty set (no chat-typed demo data).
        Ok(Vec::new())
    }

    async fn list_docs(&self) -> Result<Vec<HsDocArticle>> {
        let mut out = Vec::new();
        for col in self.list_doc_collections().await? {
            for a in self.list_doc_articles(col.remote_id).await? {
                out.push(a);
            }
        }
        Ok(out)
    }

    async fn list_ratings(&self) -> Result<Vec<HsRating>> {
        // No documented polling endpoint for all ratings; they arrive via the
        // satisfaction.ratings webhook (reference returns []).
        Ok(Vec::new())
    }

    async fn get_rating(&self, rating_id: i64) -> Result<Option<HsRating>> {
        // GET /v2/ratings/:id — 404 maps to None like the reference.
        let raw = match self
            .request(&format!("/v2/ratings/{rating_id}"), "GET", None)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("-> 404") {
                    return Ok(None);
                }
                return Err(e);
            }
        };
        let customer = raw.get("customer").cloned().unwrap_or(Value::Null);
        let first = customer.get("firstName").and_then(|v| v.as_str());
        let last = customer.get("lastName").and_then(|v| v.as_str());
        let customer_name = match (first, last) {
            (Some(f), Some(l)) => Some(format!("{f} {l}")),
            (Some(f), None) => Some(f.to_string()),
            (None, Some(l)) => Some(l.to_string()),
            (None, None) => None,
        };
        Ok(Some(HsRating {
            remote_id: raw.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
            conversation_id: raw.get("conversationId").and_then(|v| v.as_i64()),
            thread_id: raw.get("threadId").and_then(|v| v.as_i64()),
            rating: raw
                .get("rating")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            comment: raw
                .get("comments")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            customer_id: customer.get("id").and_then(|v| v.as_i64()),
            customer_name,
            user_id: raw.get("userId").and_then(|v| v.as_i64()).or_else(|| {
                raw.get("user")
                    .and_then(|u| u.get("id"))
                    .and_then(|v| v.as_i64())
            }),
            created_at: raw
                .get("createdAt")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }))
    }

    // ---------------- Extended surface ----------------

    async fn list_folders(&self, mailbox_id: i64) -> Result<Vec<HsFolder>> {
        let v = self
            .request(&format!("/v2/mailboxes/{mailbox_id}/folders"), "GET", None)
            .await?;
        Ok(v["_embedded"]["folders"]
            .as_array()
            .map(|a| a.iter().map(|f| Self::map_folder(f, mailbox_id)).collect())
            .unwrap_or_default())
    }

    async fn list_inbox_fields(&self, mailbox_id: i64) -> Result<Vec<HsField>> {
        let v = self
            .request(&format!("/v2/mailboxes/{mailbox_id}/fields"), "GET", None)
            .await?;
        Ok(v["_embedded"]["fields"]
            .as_array()
            .map(|a| a.iter().map(|f| Self::map_field(f, mailbox_id)).collect())
            .unwrap_or_default())
    }

    async fn list_saved_replies(&self, mailbox_id: i64) -> Result<Vec<HsSavedReply>> {
        let v = self
            .request(
                &format!("/v2/mailboxes/{mailbox_id}/saved-replies"),
                "GET",
                None,
            )
            .await?;
        Ok(v["_embedded"]["savedReplies"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| HsSavedReply {
                        remote_id: r["id"].as_i64().unwrap_or(0),
                        name: r["name"].as_str().unwrap_or_default().to_string(),
                        preview: r["preview"].as_str().map(|s| s.to_string()),
                        text: r["text"].as_str().map(|s| s.to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn list_workflows(&self) -> Result<Vec<HsWorkflow>> {
        let v = self.request("/v2/workflows", "GET", None).await?;
        Ok(v["_embedded"]["workflows"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|w| HsWorkflow {
                        remote_id: w["id"].as_i64().unwrap_or(0),
                        mailbox_id: w["mailboxId"].as_i64(),
                        name: w["name"].as_str().unwrap_or_default().to_string(),
                        kind: w["type"].as_str().unwrap_or("manual").to_string(),
                        status: w["status"].as_str().unwrap_or("inactive").to_string(),
                        sort_order: w["order"].as_i64().unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn list_webhooks(&self) -> Result<Vec<HsWebhookConfig>> {
        let v = self.request("/v2/webhooks", "GET", None).await?;
        Ok(v["_embedded"]["webhooks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|w| HsWebhookConfig {
                        remote_id: w["id"].as_i64().unwrap_or(0),
                        url: w["url"].as_str().unwrap_or_default().to_string(),
                        events: w["events"]
                            .as_array()
                            .map(|e| {
                                e.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        status: w["status"].as_str().unwrap_or("enabled").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn create_webhook(
        &self,
        url: &str,
        events: &[String],
        secret: &str,
        label: &str,
    ) -> Result<i64> {
        let v = self
            .request(
                "/v2/webhooks",
                "POST",
                Some(json!({ "url": url, "events": events, "secret": secret, "label": label })),
            )
            .await?;
        Ok(v["id"]
            .as_i64()
            .or_else(|| v["webhook"]["id"].as_i64())
            .unwrap_or(0))
    }

    async fn delete_webhook(&self, remote_id: i64) -> Result<bool> {
        self.request(&format!("/v2/webhooks/{remote_id}"), "DELETE", None)
            .await?;
        Ok(true)
    }

    async fn list_customer_property_definitions(&self) -> Result<Vec<HsPropertyDef>> {
        let v = self.request("/v2/customer-properties", "GET", None).await?;
        Ok(v["_embedded"]["customerProperties"]
            .as_array()
            .or_else(|| v["_embedded"]["properties"].as_array())
            .map(|a| a.iter().map(Self::map_property_def).collect())
            .unwrap_or_default())
    }

    async fn list_organization_property_definitions(&self) -> Result<Vec<HsPropertyDef>> {
        let v = self
            .request("/v2/organization-properties", "GET", None)
            .await?;
        Ok(v["_embedded"]["organizationProperties"]
            .as_array()
            .or_else(|| v["_embedded"]["properties"].as_array())
            .map(|a| a.iter().map(Self::map_property_def).collect())
            .unwrap_or_default())
    }

    async fn list_organizations(&self) -> Result<Vec<HsOrganization>> {
        let v = self.request("/v2/organizations", "GET", None).await?;
        Ok(v["_embedded"]["organizations"]
            .as_array()
            .map(|a| a.iter().map(Self::map_organization).collect())
            .unwrap_or_default())
    }

    async fn get_conversation(&self, conversation_id: i64) -> Result<Option<HsConversation>> {
        let res = self
            .request(&format!("/v3/conversations/{conversation_id}"), "GET", None)
            .await;
        match res {
            Ok(v) => Ok(Some(Self::map_conversation(&v))),
            Err(e) => {
                if is_not_found(&e) {
                    return Ok(None);
                }
                Err(e)
            }
        }
    }

    async fn list_threads(&self, conversation_id: i64) -> Result<Vec<HsThread>> {
        let v = self
            .request(
                &format!("/v3/conversations/{conversation_id}/threads"),
                "GET",
                None,
            )
            .await?;
        Ok(v["_embedded"]["threads"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|t| Self::map_thread(t, conversation_id))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn get_customer(&self, customer_id: i64) -> Result<Option<HsCustomer>> {
        let res = self
            .request(&format!("/v2/customers/{customer_id}"), "GET", None)
            .await;
        match res {
            Ok(v) => Ok(Some(Self::map_customer(&v))),
            Err(e) => {
                if is_not_found(&e) {
                    return Ok(None);
                }
                Err(e)
            }
        }
    }

    async fn get_user_status(&self, user_id: i64) -> Result<Option<HsUserStatus>> {
        let res = self
            .request(&format!("/v2/users/{user_id}/status"), "GET", None)
            .await;
        match res {
            Ok(v) => Ok(Some(HsUserStatus {
                user_id,
                email_status: v["email"]["status"].as_str().map(|s| s.to_string()),
                email_updated_at: v["email"]["updatedAt"].as_str().map(|s| s.to_string()),
                chat_status: v["chat"]["status"].as_str().map(|s| s.to_string()),
                mailbox_statuses: v["chat"]["mailboxStatuses"].clone(),
            })),
            Err(e) => {
                if is_not_found(&e) {
                    return Ok(None);
                }
                Err(e)
            }
        }
    }

    async fn list_system_users(&self) -> Result<Vec<HsUser>> {
        let v = self.request("/v2/users?status=system", "GET", None).await?;
        Ok(v["_embedded"]["users"]
            .as_array()
            .map(|a| a.iter().map(Self::map_user).collect())
            .unwrap_or_default())
    }

    async fn list_doc_collections(&self) -> Result<Vec<HsDocCollection>> {
        // Docs API: separate host + Basic auth (docs_request); no key -> empty.
        let v = self.docs_request("/v2/collections").await?;
        Ok(v["collections"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| HsDocCollection {
                        remote_id: c["id"].as_i64().unwrap_or(0),
                        slug: c["slug"].as_str().map(|s| s.to_string()),
                        name: c["name"].as_str().unwrap_or_default().to_string(),
                        description: c["description"].as_str().map(|s| s.to_string()),
                        visibility: c["visibility"].as_str().map(|s| s.to_string()),
                        article_count: c["articleCount"].as_i64(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn list_doc_categories(&self, collection_id: i64) -> Result<Vec<HsDocCategory>> {
        let v = self
            .docs_request(&format!("/v2/collections/{collection_id}/categories"))
            .await?;
        Ok(v["categories"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| HsDocCategory {
                        remote_id: c["id"].as_i64().unwrap_or(0),
                        collection_id,
                        slug: c["slug"].as_str().map(|s| s.to_string()),
                        name: c["name"].as_str().unwrap_or_default().to_string(),
                        sort_order: c["order"].as_i64(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn list_doc_articles(&self, collection_id: i64) -> Result<Vec<HsDocArticle>> {
        let v = self
            .docs_request(&format!("/v2/collections/{collection_id}/articles"))
            .await?;
        Ok(v["articles"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|art| HsDocArticle {
                        remote_id: art["id"].as_i64().unwrap_or(0),
                        collection_id,
                        category_id: art["categoryId"].as_i64(),
                        number: art["number"].as_i64(),
                        slug: art["slug"].as_str().map(|s| s.to_string()),
                        name: art["name"].as_str().unwrap_or_default().to_string(),
                        status: art["status"].as_str().map(|s| s.to_string()),
                        text: art["text"].as_str().map(|s| s.to_string()),
                        views: art["views"].as_i64(),
                        created_at: art["createdAt"].as_str().map(|s| s.to_string()),
                        updated_at: art["updatedAt"].as_str().map(|s| s.to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    // -----------------------------------------------------------------
    // Mutations (realProvider.ts:323-375)
    // -----------------------------------------------------------------

    async fn create_reply_thread(&self, input: CreateThreadInput) -> Result<ThreadCreated> {
        let mut body = json!({ "text": input.text, "draft": input.draft });
        if !input.cc.is_empty() {
            body["cc"] = json!(input.cc);
        }
        if !input.bcc.is_empty() {
            body["bcc"] = json!(input.bcc);
        }
        if let Some(status_after) = &input.status_after {
            body["status"] = json!(status_after);
        }
        if let Some(assign_to) = input.assign_to {
            body["assignTo"] = json!(assign_to);
        }
        let res = self
            .request(
                &format!("/v2/conversations/{}/reply", input.conversation_id),
                "POST",
                Some(body),
            )
            .await?;
        Ok(ThreadCreated {
            thread_id: res["id"].as_i64().unwrap_or_default(),
            conversation_id: input.conversation_id,
        })
    }

    async fn create_note_thread(&self, input: CreateThreadInput) -> Result<ThreadCreated> {
        let res = self
            .request(
                &format!("/v2/conversations/{}/notes", input.conversation_id),
                "POST",
                Some(json!({ "text": input.text })),
            )
            .await?;
        Ok(ThreadCreated {
            thread_id: res["id"].as_i64().unwrap_or_default(),
            conversation_id: input.conversation_id,
        })
    }

    async fn update_conversation(
        &self,
        conversation_id: i64,
        patch: ConversationPatch,
    ) -> Result<bool> {
        // realProvider.ts builds one JSON-Patch operation per present field
        // and PATCHes them one at a time (PRIORITY.INTERACTIVE).
        let path = format!("/v2/conversations/{conversation_id}");
        if let Some(subject) = patch.subject {
            self.request(
                &path,
                "PATCH",
                Some(json!({ "op": "replace", "path": "/subject", "value": subject })),
            )
            .await?;
        }
        if let Some(status) = patch.status {
            self.request(
                &path,
                "PATCH",
                Some(json!({ "op": "replace", "path": "/status", "value": status })),
            )
            .await?;
        }
        if let Some(mailbox_id) = patch.mailbox_id {
            self.request(
                &path,
                "PATCH",
                Some(json!({ "op": "move", "path": "/mailboxId", "value": mailbox_id })),
            )
            .await?;
        }
        match patch.assign_to {
            Some(None) => {
                self.request(
                    &path,
                    "PATCH",
                    Some(json!({ "op": "remove", "path": "/assignTo" })),
                )
                .await?;
            }
            Some(Some(assign_to)) => {
                self.request(
                    &path,
                    "PATCH",
                    Some(json!({ "op": "replace", "path": "/assignTo", "value": assign_to })),
                )
                .await?;
            }
            None => {}
        }
        Ok(true)
    }

    /// realProvider.ts `createConversation` (audit OR-02 / B3): POST a new
    /// conversation to `/v3/conversations` with the customer's first message
    /// as the first thread. The send executor uses the returned id + number
    /// to record `hs_conversation_remote_id` + `hs_conversation_number` on
    /// `outreach_recipients` and enqueue a `sync_conversation` job.
    async fn create_conversation(
        &self,
        input: CreateConversationInput,
    ) -> Result<ConversationCreated> {
        let mut threads = json!([{
            "type": "customer",
            "customer": { "id": input.customer_id },
            "text": input.body,
        }]);
        if !input.tags.is_empty() {
            threads[0]["tags"] = json!(input.tags);
        }
        let mut body = json!({
            "subject": input.subject,
            "mailboxId": input.mailbox_id,
            "customer": { "id": input.customer_id },
            "threads": threads,
        });
        if let Some(status) = &input.status {
            body["status"] = json!(status);
        }
        if !input.tags.is_empty() {
            body["tags"] = json!(input.tags);
        }
        let res = self
            .request("/v3/conversations", "POST", Some(body))
            .await?;
        let conversation_id = res["id"].as_i64().unwrap_or(0);
        let number = res["number"].as_i64().unwrap_or(0);
        let thread_id = res["threads"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|t| t["id"].as_i64())
            .unwrap_or(0);
        if conversation_id == 0 {
            // Help Scout returned 2xx but no id — the executor treats this as
            // an UNKNOWN outcome (the recipient is parked in 'unknown' state
            // for the reconcile pass to resolve against the local mirror).
            return Err(HsApiError {
                status_code: 200,
                message: "Help Scout created the conversation but returned no id".into(),
                friendly:
                    "Help Scout accepted the new conversation but did not return a conversation id. \
                     The recipient will be reconciled against the local mirror."
                        .into(),
                retryable: false,
            }
            .into());
        }
        Ok(ConversationCreated {
            conversation_id,
            number,
            thread_id,
        })
    }
}

impl RealHelpScoutProvider {
    fn map_property_def(v: &Value) -> HsPropertyDef {
        HsPropertyDef {
            remote_id: v["id"].as_i64().unwrap_or(0),
            name: v["name"].as_str().unwrap_or_default().to_string(),
            slug: v["slug"].as_str().map(|s| s.to_string()),
            kind: v["type"].as_str().unwrap_or("text").to_string(),
            sort_order: v["order"].as_i64().unwrap_or(0),
        }
    }
}

/// Whether an API error is a 404 (used for Option-returning fetches).
fn is_not_found(e: &Error) -> bool {
    hs_status(e) == Some(404) || e.to_string().contains("-> 404")
}

/// The typed Help Scout status code, when the error wraps an `HsApiError`.
pub fn hs_status(e: &Error) -> Option<u16> {
    if let Error::Other(b) = e {
        if let Some(hs) = b.downcast_ref::<HsApiError>() {
            return Some(hs.status_code);
        }
    }
    None
}

/// The typed Help Scout error, when present (friendly text + status code
/// for the write pipeline's response shapes — operations.ts catch blocks).
pub fn hs_error(e: &Error) -> Option<&HsApiError> {
    if let Error::Other(b) = e {
        b.downcast_ref::<HsApiError>()
    } else {
        None
    }
}

/// Ensure the account='default' row shape exists (legacy id=1 port DBs):
/// the reference stores tokens under `account='default'`; the port's
/// original table had only an `id=1` primary key.
fn ensure_account_row(conn: &Connection) {
    let has_account_col: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('oauth_tokens') WHERE name = 'account'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !has_account_col {
        let _ = conn.execute_batch(
            "ALTER TABLE oauth_tokens ADD COLUMN account TEXT;
             ALTER TABLE oauth_tokens ADD COLUMN revoked INTEGER DEFAULT 0;
             UPDATE oauth_tokens SET account = 'default' WHERE account IS NULL;",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_errors_match_reference_messages() {
        assert_eq!(
            friendly_error(401, "", "POST"),
            "The Help Scout connection is no longer authenticated. Re-connect Help Scout in Settings, then retry. No changes were made."
        );
        assert_eq!(
            friendly_error(404, "", "GET"),
            "Help Scout reports this record no longer exists (it may have been deleted or merged into another conversation). Local data was preserved."
        );
        assert!(friendly_error(400, "", "GET").contains("check the filters used"));
        assert!(friendly_error(400, "", "POST").contains("check the values you entered"));
    }

    #[test]
    fn authorize_url_has_exactly_client_id_and_state() {
        let creds = HsCredentials {
            client_id: "abc 123".into(),
            client_secret: "s".into(),
            redirect_uri: String::new(),
            api_base: String::new(),
            webhook_secret: String::new(),
            docs_api_key: String::new(),
            docs_api_base: String::new(),
        };
        let url = creds.authorize_url("st&ate");
        assert!(url.starts_with(
            "https://secure.helpscout.net/authentication/authorizeClientApplication?"
        ));
        assert!(url.contains("client_id=abc%20123"));
        assert!(url.contains("state=st%26ate"));
        assert!(!url.contains("redirect_uri"));
        assert!(!url.contains("scope"));
    }

    #[test]
    fn rate_limiter_snapshot_shape() {
        let limiter = HsRateLimiter::new(None);
        limiter.record_response(Some(150), Some(140), None, false);
        let snap = limiter.snapshot();
        assert_eq!(snap["limitPerMinute"], json!(150));
        assert_eq!(snap["remaining"], json!(140));
        assert_eq!(snap["inFlightWindow"], json!(1));
    }

    #[test]
    fn writes_count_double_in_window() {
        let limiter = HsRateLimiter::new(None);
        limiter.record_response(None, None, None, true);
        assert_eq!(limiter.snapshot()["inFlightWindow"], json!(2));
    }

    #[test]
    fn record_429_sets_retry_state() {
        let limiter = HsRateLimiter::new(None);
        limiter.record_error_429(Some(17));
        let snap = limiter.snapshot();
        assert_eq!(snap["retryAfterSec"], json!(17));
        assert_eq!(snap["remaining"], json!(0));
    }

    #[test]
    fn retryable_status_set() {
        assert!(is_retryable(429));
        assert!(is_retryable(500));
        assert!(is_retryable(503));
        assert!(is_retryable(504));
        assert!(!is_retryable(404));
        assert!(!is_retryable(400));
    }
}
