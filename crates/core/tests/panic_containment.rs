//! C3 (audit T16): panic containment at the HTTP handler boundary. With the
//! release profile previously on `panic = "abort"`, ANY unrecovered panic in
//! a route handler (the operations tile invariant, a poisoned mutex, ...)
//! killed the whole packaged app. The router now wraps every handler in
//! tower-http's `CatchPanicLayer` (release profile: unwind), converting the
//! panic into the port's standard 500 envelope while the server keeps
//! serving.
//!
//! This boots a real loopback server (same pattern as cors_origins.rs, but
//! with a minimal router whose single route panics on purpose) using the
//! SAME `catch_panic_response` the production router installs, then probes:
//!
//!   - a panicking handler answers 500 with the
//!     {statusCode:500, error:"InternalError", message} envelope;
//!   - the server keeps serving afterwards (the next request succeeds).

use std::time::Duration;

use axum::routing::get;
use axum::Router;
use tower_http::catch_panic::CatchPanicLayer;

const PORT: u16 = 3991;

// Multi-thread runtime: the server task must keep running while the test
// drives it from the test thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handler_panics_answer_500_and_server_survives() {
    // A router shaped like the production one: the C3 CatchPanicLayer with
    // the same response builder the real router installs, around a handler
    // that panics unconditionally.
    async fn boom() -> &'static str {
        panic!("tile invariant violated")
    }
    let app = Router::new()
        .route("/api/boom", get(boom))
        .route("/api/health", get(|| async { "ok" }))
        .layer(CatchPanicLayer::custom(
            spp_core::http::server::catch_panic_response,
        ));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT))
        .await
        .expect("bind");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server");
    });
    // Give the listener a moment to be ready.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let base = format!("http://127.0.0.1:{PORT}");

    // 1. The panicking handler answers the standard 500 envelope — not a
    //    connection reset, not an aborted process.
    let resp = reqwest::get(format!("{base}/api/boom"))
        .await
        .expect("request");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["statusCode"], 500);
    assert_eq!(body["error"], "InternalError");
    assert_eq!(
        body["message"].as_str().expect("message"),
        "Internal error: tile invariant violated"
    );

    // 2. The server keeps serving afterwards.
    for i in 0..5 {
        let resp = reqwest::get(format!("{base}/api/health"))
            .await
            .expect("health");
        assert_eq!(resp.status(), 200, "still serving after panic #{i}");
        assert_eq!(resp.text().await.expect("text"), "ok");
    }

    server.abort();
}
