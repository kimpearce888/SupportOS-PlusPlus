//! SY-07 (audit M21): the OAuth flow on the real HTTP server — the five
//! reference endpoints (routes/sync.ts:269-385 + authService.ts).
//!
//! The audit flagged the dead M2-era machinery: a Tauri loopback receiver
//! that bound a listener and dropped it, `{ok:true, todo:M2}` handler stubs
//! and a placeholder `exchange_code` that only worked for the literal code
//! `"test_code"`. The live flow now rides the main HTTP server
//! (`http/routes/oauth.rs`) with the single-use CSRF state in
//! `application_settings`, code exchange through
//! `RealHelpScoutProvider::exchange_code`, and the legacy path deleted.
//!
//! This boots the REAL server twice and probes with real HTTP:
//!
//! Demo boot (real provider absent):
//!   - GET  /api/oauth/authorize-url  -> {demo_mode:true, message}
//!   - POST /api/oauth/client-credentials -> {demo_mode:true}
//!   - GET  /api/oauth/status -> {configured:false, authenticated:true,
//!     demo_mode:true, expires_at:null, me:null}
//!   - POST /api/oauth/disconnect -> ok:true + helpscout_disconnected audit
//!   - GET  /oauth/callback -> the demo fail page
//!
//! Real boot (configured credentials + a local mock Help Scout token/user
//! endpoint):
//!   - GET /api/oauth/authorize-url -> the reference authorize URL with
//!     client_id + a fresh 32-hex state persisted in application_settings;
//!   - the state is SINGLE-USE: a callback with a wrong state is refused
//!     and the stored state is consumed;
//!   - the callback with the right state exchanges the code against the
//!     mock, stores the token, records me_remote_id + the
//!     helpscout_connected audit row and serves the connected HTML page;
//!   - GET /api/oauth/status -> configured + authenticated + me{name,email};
//!   - POST /api/oauth/disconnect -> revoked (status authenticated:false).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{json, Value};

const DEMO_PORT: u16 = 3996;
const REAL_PORT: u16 = 3997;
const MOCK_PORT: u16 = 3998;

fn boot_server(port: u16, real: Option<Arc<spp_core::helpscout_real::RealHelpScoutProvider>>) {
    let real_kind = real.is_some();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    let http_conn = Arc::new(Mutex::new(conn));
    let bus = spp_core::http::EventBus::default();
    let provider = Arc::new(spp_core::helpscout::FakeHelpScoutProvider::new_demo())
        as Arc<dyn spp_core::helpscout::HelpScoutProvider>;
    let sync = Arc::new(
        spp_core::sync_engine::SyncEngine::new(http_conn.clone(), provider).with_bus(bus.clone()),
    );
    let qdrant = spp_core::http::server::AppState::qdrant_from_settings(
        &http_conn.lock().unwrap_or_else(|p| p.into_inner()),
        &data_dir,
    );
    let state = spp_core::http::server::AppState {
        conn: http_conn,
        data_dir,
        port,
        host: "127.0.0.1".to_string(),
        demo_mode: real.is_none(),
        bus,
        limiter: spp_core::http::RateLimiter::new(),
        sync: Some(sync),
        real,
        provider_kind: if real_kind { "real" } else { "fake" }.to_string(),
        workers: None,
        qdrant,
    };
    let server = spp_core::http::HttpServer::new(state);
    tokio::spawn(async move {
        if let Err(e) = server.serve().await {
            eprintln!("HTTP server failed: {e}");
        }
    });
}

async fn wait_up(client: &reqwest::Client, base: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(r) = client.get(format!("{base}/health")).send().await {
            if r.status().as_u16() == 200 {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "server did not come up on {base}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn mock_credentials() -> spp_core::helpscout_real::HsCredentials {
    use spp_core::helpscout_real::HsCredentials;
    HsCredentials {
        client_id: "cid".into(),
        client_secret: "csecret".into(),
        redirect_uri: format!("http://127.0.0.1:{REAL_PORT}/oauth/callback"),
        api_base: format!("http://127.0.0.1:{MOCK_PORT}"),
        webhook_secret: String::new(),
        docs_api_key: String::new(),
        docs_api_base: String::new(),
    }
}

/// Demo-mode contract of all five endpoints (reference demo early-returns).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_mode_oauth_contract_matches_reference() {
    boot_server(DEMO_PORT, None);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    let base = format!("http://127.0.0.1:{DEMO_PORT}");
    wait_up(&client, &base).await;

    // authorize-url: demo early-return.
    let v: Value = client
        .get(format!("{base}/api/oauth/authorize-url"))
        .send()
        .await
        .expect("authorize-url")
        .json()
        .await
        .expect("json");
    assert_eq!(v["demo_mode"], true);
    assert_eq!(
        v["message"].as_str().expect("message"),
        "Demo mode is active - OAuth is not needed."
    );

    // client-credentials: demo early-return.
    let v: Value = client
        .request(Method::POST, format!("{base}/api/oauth/client-credentials"))
        .send()
        .await
        .expect("client-credentials")
        .json()
        .await
        .expect("json");
    assert_eq!(v["demo_mode"], true);

    // status: the reference demo shape.
    let v: Value = client
        .get(format!("{base}/api/oauth/status"))
        .send()
        .await
        .expect("status")
        .json()
        .await
        .expect("json");
    assert_eq!(v["configured"], false);
    assert_eq!(v["authenticated"], true);
    assert_eq!(v["demo_mode"], true);
    assert_eq!(v["expires_at"], Value::Null);
    assert_eq!(v["me"], Value::Null);

    // disconnect: still ok + audited even in demo mode.
    let v: Value = client
        .request(Method::POST, format!("{base}/api/oauth/disconnect"))
        .send()
        .await
        .expect("disconnect")
        .json()
        .await
        .expect("json");
    assert_eq!(v["ok"], true);
    assert_eq!(
        v["message"].as_str().expect("message"),
        "Disconnected. Local data is fully preserved."
    );

    // callback: the demo fail page (HTML, reference copy).
    let resp = client
        .get(format!("{base}/oauth/callback"))
        .send()
        .await
        .expect("callback");
    assert_eq!(resp.status().as_u16(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(ct.starts_with("text/html"), "content-type {ct}");
    let body = resp.text().await.expect("body");
    assert!(body.contains("Help Scout connection not completed"));
    assert!(body.contains("Demo mode is active - OAuth is not needed."));
}

/// The full code flow against a configured (mock-backed) real provider:
/// authorize-url state issue -> single-use verification -> code exchange ->
/// token storage -> me -> status -> disconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_provider_code_flow_completes_end_to_end() {
    // ── Local mock Help Scout token + user endpoints ───────────────────────
    let mock = tokio::spawn(async move {
        use axum::routing::{get, post};
        let app = axum::Router::new()
            .route(
                "/v2/oauth2/token",
                post(|| async {
                    axum::Json(json!({
                        "access_token": "at_live_123",
                        "refresh_token": "rt_live_456",
                        "expires_in": 172800,
                        "token_type": "bearer"
                    }))
                }),
            )
            .route(
                "/v2/users/me",
                get(|| async {
                    axum::Json(json!({
                        "id": 77,
                        "firstName": "Ada",
                        "lastName": "Lovelace",
                        "email": "ada@example.com",
                        "role": "owner",
                        "type": "user"
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", MOCK_PORT))
            .await
            .expect("mock bind");
        axum::serve(listener, app).await.expect("mock server");
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // ── Boot the server with a configured real provider ─────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");
    let http_conn = Arc::new(Mutex::new(conn));
    let real = Arc::new(spp_core::helpscout_real::RealHelpScoutProvider::new(
        http_conn.clone(),
        mock_credentials(),
    ));
    let bus = spp_core::http::EventBus::default();
    let provider = Arc::new(spp_core::helpscout::FakeHelpScoutProvider::new_demo())
        as Arc<dyn spp_core::helpscout::HelpScoutProvider>;
    let sync = Arc::new(
        spp_core::sync_engine::SyncEngine::new(http_conn.clone(), provider).with_bus(bus.clone()),
    );
    let qdrant = spp_core::http::server::AppState::qdrant_from_settings(
        &http_conn.lock().unwrap_or_else(|p| p.into_inner()),
        &data_dir,
    );
    let state = spp_core::http::server::AppState {
        conn: http_conn.clone(),
        data_dir,
        port: REAL_PORT,
        host: "127.0.0.1".to_string(),
        demo_mode: false,
        bus,
        limiter: spp_core::http::RateLimiter::new(),
        sync: Some(sync),
        real: Some(real),
        provider_kind: "real".to_string(),
        workers: None,
        qdrant,
    };
    let server = spp_core::http::HttpServer::new(state);
    tokio::spawn(async move {
        if let Err(e) = server.serve().await {
            eprintln!("HTTP server failed: {e}");
        }
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("client");
    let base = format!("http://127.0.0.1:{REAL_PORT}");
    wait_up(&client, &base).await;

    // 1. authorize-url: the reference URL + persisted single-use state.
    let v: Value = client
        .get(format!("{base}/api/oauth/authorize-url"))
        .send()
        .await
        .expect("authorize-url")
        .json()
        .await
        .expect("json");
    let url = v["url"].as_str().expect("url").to_string();
    assert!(
        url.starts_with(
            "https://secure.helpscout.net/authentication/authorizeClientApplication?client_id=cid&state="
        ),
        "authorize url shape: {url}"
    );
    let state_param = url.split("state=").nth(1).expect("state param").to_string();
    assert_eq!(state_param.len(), 32, "16 random bytes hex-encoded");
    {
        let stored: Option<String> = http_conn
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .query_row(
                "SELECT value FROM application_settings WHERE key = 'oauth_state'",
                [],
                |r| r.get(0),
            )
            .ok();
        let stored = stored
            .and_then(|s| serde_json::from_str::<String>(&s).ok())
            .expect("oauth_state stored (JSON-wrapped)");
        assert_eq!(stored, state_param);
    }

    // 2. Single-use CSRF state: a WRONG state is refused, and the stored
    //    state is consumed (the next right state no longer works either).
    let resp = client
        .get(format!("{base}/oauth/callback?code=abc&state=deadbeef"))
        .send()
        .await
        .expect("wrong-state callback");
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.expect("body");
    assert!(
        body.contains("The state parameter did not match"),
        "state mismatch page: {body}"
    );
    {
        let gone: Option<String> = http_conn
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .query_row(
                "SELECT value FROM application_settings WHERE key = 'oauth_state'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert!(gone.is_none(), "the stored state was consumed (single-use)");
    }

    // 3. The real flow: fresh state -> callback with the RIGHT state.
    let v: Value = client
        .get(format!("{base}/api/oauth/authorize-url"))
        .send()
        .await
        .expect("authorize-url 2")
        .json()
        .await
        .expect("json");
    let state2 = v["url"]
        .as_str()
        .expect("url")
        .split("state=")
        .nth(1)
        .expect("state")
        .to_string();
    let resp = client
        .get(format!(
            "{base}/oauth/callback?code=good_code&state={state2}"
        ))
        .send()
        .await
        .expect("right-state callback");
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.expect("body");
    assert!(body.contains("Connected to Help Scout"), "page: {body}");
    assert!(body.contains("Connected as Ada Lovelace"));
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        // Token stored from the mock exchange.
        let (access, refresh): (String, Option<String>) = conn
            .query_row(
                "SELECT access_token, refresh_token FROM oauth_tokens WHERE account='default'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("token row");
        assert_eq!(access, "at_live_123");
        assert_eq!(refresh.as_deref(), Some("rt_live_456"));
        // me_remote_id persisted.
        let me: Option<String> = conn
            .query_row(
                "SELECT value FROM application_settings WHERE key = 'me_remote_id'",
                [],
                |r| r.get(0),
            )
            .ok();
        let me = me.and_then(|s| serde_json::from_str::<i64>(&s).ok());
        assert_eq!(me, Some(77), "me_remote_id = the mock user id");
        // Audit row recorded with the reference remote_operation string.
        let audited: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action='helpscout_connected'
                  AND remote_operation='GET /oauth/callback (code exchange)'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        assert!(audited >= 1, "helpscout_connected audit row written");
    }

    // 4. status: configured + authenticated + me.
    let v: Value = client
        .get(format!("{base}/api/oauth/status"))
        .send()
        .await
        .expect("status")
        .json()
        .await
        .expect("json");
    assert_eq!(v["configured"], true);
    assert_eq!(v["authenticated"], true);
    assert_eq!(v["demoMode"], false);
    assert!(v["expiresAt"].as_str().is_some(), "expires_at served");
    assert_eq!(v["me"]["name"], "Ada Lovelace");
    assert_eq!(v["me"]["email"], "ada@example.com");

    // 5. disconnect: revoke -> status flips to unauthenticated.
    let v: Value = client
        .request(Method::POST, format!("{base}/api/oauth/disconnect"))
        .send()
        .await
        .expect("disconnect")
        .json()
        .await
        .expect("json");
    assert_eq!(v["ok"], true);
    let v: Value = client
        .get(format!("{base}/api/oauth/status"))
        .send()
        .await
        .expect("status 2")
        .json()
        .await
        .expect("json");
    assert_eq!(v["configured"], true);
    assert_eq!(v["authenticated"], false, "revoked after disconnect");
    assert_eq!(v["me"], Value::Null);

    mock.abort();
}
