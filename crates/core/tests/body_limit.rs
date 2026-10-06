//! TH-09 (audit M19): the router enforces the reference's request-body
//! limit — Fastify's `bodyLimit: 20 * 1024 * 1024` (app.ts:76, sized for
//! base64-inflated attachment uploads). The production router installs
//! `axum::extract::DefaultBodyLimit::max(20 MB)` globally, with a per-route
//! 512 MB override on the .sosync octet-stream upload route (the reference's
//! `{ bodyLimit: 512 * 1024 * 1024 }`, routes/sync.ts:241).
//!
//! This boots the REAL HTTP server (same pattern as cors_origins.rs) and
//! probes the two layers with real HTTP requests:
//!
//!   - a > 20 MB JSON body on a regular API route is rejected with 413
//!     before any handler logic runs;
//!   - the SAME payload size passes the .sosync upload route's per-route
//!     override (it reaches the handler and is rejected by the magic-byte
//!     check instead — proving the 512 MB override beats the global limit);
//!   - a small body is unaffected (no 413 anywhere).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Method;

const PORT: u16 = 3994;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_limit_20mb_rejects_oversized_json_but_upload_override_allows_it() {
    // ── Boot the real server (mirrors the Tauri shell) ──────────────────────
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
        .timeout(Duration::from_secs(60))
        // A 413 response leaves the request body undrained, so the server
        // closes that connection; never reuse pooled connections here or a
        // follow-up request can race a stale socket (BrokenPipe).
        .pool_max_idle_per_host(0)
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

    // ── 1. A > 20 MB JSON body is rejected with 413 on a regular route ─────
    // 21 MB of padding inside a JSON string — over the 20 MB default limit.
    let big_padding = "a".repeat(21 * 1024 * 1024);
    let big_json = format!(r#"{{"title":"x","firstMessage":"{big_padding}"}}"#);
    let resp = client
        .request(
            Method::POST,
            format!("{base}/api/conversations/1/side-threads"),
        )
        .header("Content-Type", "application/json")
        .body(big_json)
        .send()
        .await
        .expect("oversized POST");
    assert_eq!(
        resp.status().as_u16(),
        413,
        "a >20MB JSON body must be rejected with 413 (the reference's bodyLimit)"
    );

    // ── 2. The .sosync upload route's 512 MB override lets the same size
    //       through: 21 MB of non-SOSYNC garbage reaches the HANDLER and is
    //       rejected by the magic-byte check (422), not by the body limit ──
    let garbage = vec![0x41u8; 21 * 1024 * 1024];
    let resp = client
        .request(Method::POST, format!("{base}/api/sync/encrypted/upload"))
        .header("Content-Type", "application/octet-stream")
        .body(garbage)
        .send()
        .await
        .expect("oversized upload");
    assert_ne!(
        resp.status().as_u16(),
        413,
        "the .sosync upload route must NOT hit the global 20 MB limit (512 MB per-route override)"
    );
    assert_eq!(
        resp.status().as_u16(),
        422,
        "21 MB of non-SOSYNC bytes must reach the handler and fail the magic check"
    );

    // ── 3. Small bodies are unaffected ──────────────────────────────────────
    let resp = client
        .request(
            Method::POST,
            format!("{base}/api/conversations/1/side-threads"),
        )
        .header("Content-Type", "application/json")
        .body(r#"{"title":"small"}"#)
        .send()
        .await
        .expect("small POST");
    assert_ne!(
        resp.status().as_u16(),
        413,
        "a small JSON body must never be rejected by the body limit"
    );
}
