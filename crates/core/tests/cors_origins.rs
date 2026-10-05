//! Audit item UI-23 / blocker B1: the packaged Tauri webview's origins must be
//! allowed by the loopback server's CORS layer. Boots the REAL HTTP server
//! exactly the way the Tauri shell does (like tests/e2e_demo_boot.rs) and
//! probes the CORS behavior with real HTTP requests:
//!
//!   - every allowlisted origin (loopback names on the configured port, Vite
//!     dev ports, Tauri webview origins, Trunk :1420 dev server) gets
//!     `Access-Control-Allow-Origin` echoed back;
//!   - a foreign origin is still served (200) but receives NO CORS headers —
//!     matching the reference's deny-without-throw semantics (app.ts
//!     `cb(null, false)`: the browser blocks the read);
//!   - a request without an Origin header (same-origin / non-browser client)
//!     is served with no CORS headers;
//!   - a preflight from the packaged webview origin is answered with the
//!     allowed methods.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Method;

const PORT: u16 = 3987;

// Multi-thread runtime: the server's async task must keep running while the
// test drives it from the test thread (same rationale as e2e_demo_boot.rs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cors_allows_packaged_tauri_webview_origins() {
    // ── Boot, mirroring the Tauri shell (app/src-tauri/src/lib.rs) ──────────
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
        conn: http_conn.clone(),
        data_dir: data_dir.clone(),
        port: PORT,
        host: "127.0.0.1".to_string(),
        demo_mode: true,
        bus,
        limiter: spp_core::http::RateLimiter::new(),
        sync: Some(sync),
        real: None,
        provider_kind: "fake".to_string(),
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
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");
    let base = format!("http://127.0.0.1:{PORT}");

    // ── Wait for boot ────────────────────────────────────────────────────────
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(r) = client.get(format!("{base}/health")).send().await {
            if r.status().as_u16() == 200 {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "server did not come up on port {PORT}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // ── 1. Allowlisted origins get the origin echoed back ───────────────────
    let allowed = [
        // Reference-parity allowlist (loopback names + Vite dev ports).
        format!("http://localhost:{PORT}"),
        format!("http://127.0.0.1:{PORT}"),
        "http://localhost:5173".to_string(),
        "http://127.0.0.1:5175".to_string(),
        // Tauri webview origins — the B1/UI-23 fix under test.
        "http://tauri.localhost".to_string(),
        "https://tauri.localhost".to_string(),
        "tauri://localhost".to_string(),
        "http://127.0.0.1:1420".to_string(),
        "http://localhost:1420".to_string(),
    ];
    for origin in &allowed {
        let r = client
            .get(format!("{base}/health"))
            .header("Origin", origin.as_str())
            .send()
            .await
            .expect("request with allowlisted origin");
        assert_eq!(
            r.status().as_u16(),
            200,
            "GET /health must stay served for Origin {origin}"
        );
        let acao = r
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok());
        assert_eq!(
            acao,
            Some(origin.as_str()),
            "ACAO must echo allowlisted origin {origin}"
        );
    }

    // ── 2. Foreign origin: served (200) but NO CORS headers ─────────────────
    let r = client
        .get(format!("{base}/health"))
        .header("Origin", "http://evil.example")
        .send()
        .await
        .expect("request with foreign origin");
    assert_eq!(r.status().as_u16(), 200, "foreign origin is still served");
    assert!(
        r.headers().get("access-control-allow-origin").is_none(),
        "foreign origin must NOT receive ACAO (deny-without-throw)"
    );

    // ── 3. No Origin header (same-origin / curl): served, no CORS headers ───
    let r = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("request without origin");
    assert_eq!(r.status().as_u16(), 200);
    assert!(
        r.headers().get("access-control-allow-origin").is_none(),
        "no-Origin request must not receive ACAO"
    );

    // ── 4. Preflight from the packaged webview origin is answered ───────────
    let r = client
        .request(Method::OPTIONS, format!("{base}/api/conversations"))
        .header("Origin", "http://tauri.localhost")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .await
        .expect("preflight request");
    let status = r.status().as_u16();
    assert!(
        status == 200 || status == 204,
        "preflight must succeed, got {status}"
    );
    let acao = r
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        acao,
        Some("http://tauri.localhost"),
        "preflight from the packaged webview must be allowed"
    );
    let methods = r
        .headers()
        .get("access-control-allow-methods")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_uppercase();
    assert!(
        methods.contains("POST"),
        "preflight allow-methods must include POST, got: {methods}"
    );
}
