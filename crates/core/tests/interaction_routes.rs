//! Audit item AI-19 / C7: interaction routes (GET card/evidence/profile,
//! refresh).
//!
//! The audit found the interactions routes querying a FICTIONAL schema:
//! `interaction_signals` with columns that do not exist (conversation_id /
//! signal_type / confidence / created_at — prepare failed and was swallowed
//! into an empty list), an `interaction_evidence` table that was never
//! created, a refresh route that answered ok:true without touching any
//! data, and a profile join through the same fictional columns.
//!
//! This test boots the REAL HTTP server and proves the fixed routes serve
//! REAL data from the reference-shaped engine layer:
//!
//!   - GET /api/interaction/:id — the stored current-signals snapshot
//!     (empty before refresh, REAL deterministic signals after).
//!   - POST /api/interaction/:id/refresh — recomputes + persists the
//!     snapshot and the evidence rows, serves the fresh signals.
//!   - GET /api/interaction/:id/evidence — the persisted evidence rows with
//!     parsed evidence_data (excerpt + thread pointers + signal context).
//!   - GET /api/interaction/profile/:customerId — the aggregated profile
//!     from the signals layer, with the human override taking precedence.
//!   - 404 reference envelopes for unknown conversations/customers on all
//!     four routes.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PORT: u16 = 3985;

// Multi-thread runtime: the server's async task must keep running while the
// test thread drives the HTTP probes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interaction_routes_serve_real_signals_evidence_and_profile() {
    spp_core::logging::init();

    // ── Boot the real server against a throwaway data dir ─────────────────
    // Distinctive remote ids (99xxx) so the fixture rows can never collide
    // with the demo world's 105xxx range.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("interaction-routes.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");
    // Customer 500 (remote 99001).
    conn.execute(
        "INSERT INTO customers (id, remote_id, first_name, last_name, created_at, updated_at)
         VALUES (500, 99001, 'Ada', 'Lovelace', datetime('now'), datetime('now'))",
        [],
    )
    .expect("seed customer");
    // Conversation 700 (remote 99700) owned by customer 500.
    conn.execute(
        "INSERT INTO conversations
            (id, remote_id, number, subject, status, mailbox_id, customer_id)
         VALUES (700, 99700, 7001, 'API returns 401', 'active', 1, 500)",
        [],
    )
    .expect("seed conversation");
    // One customer thread with deterministic markers: frustration ("still",
    // "already", "nobody", "every time"), technical language (api, endpoint,
    // 401, token) and an explicit step-by-step preference.
    conn.execute(
        "INSERT INTO conversation_threads
            (conversation_id, thread_type, state, body, actor_type, created_at)
         VALUES (700, 'customer', 'published',
            'This is STILL not working. I already called twice and nobody fixed it. Every time the same thing. Please walk me through step by step how to fix the API endpoint, the 401 says the token expired.',
            'customer', datetime('now'))",
        [],
    )
    .expect("seed customer thread");

    let http_conn = Arc::new(Mutex::new(conn));
    let bus = spp_core::http::EventBus::default();
    let qdrant = spp_core::http::server::AppState::qdrant_from_settings(
        &http_conn.lock().unwrap_or_else(|p| p.into_inner()),
        &data_dir,
    );
    let state = spp_core::http::server::AppState {
        conn: http_conn.clone(),
        data_dir,
        port: PORT,
        host: "127.0.0.1".to_string(),
        demo_mode: false,
        bus,
        limiter: spp_core::http::RateLimiter::new(),
        sync: None,
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

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(r) = client.get(format!("{base}/health")).send().await {
            if r.status().as_u16() == 200 {
                break;
            }
        }
        if Instant::now() > deadline {
            panic!("server did not boot");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // ── 1. GET before refresh: exists, but no snapshot yet ─────────────────
    // (The OLD broken code also answered empty here — the distinguishing
    // assertion comes after refresh: the snapshot must become REAL.)
    let r = client
        .get(format!("{base}/api/interaction/700"))
        .send()
        .await
        .expect("get");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["conversationId"], json!(700));
    assert!(body["signals"].as_array().is_some(), "shape: {body}");
    assert_eq!(body["signals"].as_array().unwrap().len(), 0);

    // ── 2. POST refresh: computes + persists the snapshot ─────────────────
    let r = client
        .post(format!("{base}/api/interaction/700/refresh"))
        .send()
        .await
        .expect("refresh");
    assert_eq!(r.status().as_u16(), 200, "refresh answers 200");
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["ok"], json!(true), "body: {body}");
    assert_eq!(
        body["message"], "Interaction signals refreshed.",
        "body: {body}"
    );
    let signals = body["signals"].as_array().cloned().unwrap_or_default();
    assert!(
        !signals.is_empty(),
        "refresh returns REAL computed signals: {body}"
    );
    // Every signal carries the reference fragment shape.
    for s in &signals {
        assert!(s["dimension"].as_str().is_some(), "dimension: {s}");
        assert!(s["value"].as_str().is_some(), "value: {s}");
        assert!(s["confidence"].as_str().is_some(), "confidence: {s}");
        assert_eq!(s["source"], json!("heuristic"), "source: {s}");
    }
    // The seeded markers deterministically produce these signals.
    let dims: Vec<&str> = signals
        .iter()
        .filter_map(|s| s["dimension"].as_str())
        .collect();
    assert!(dims.contains(&"frustration"), "dims: {dims:?}");
    assert!(dims.contains(&"technical_language"), "dims: {dims:?}");
    assert!(dims.contains(&"response_preference"), "dims: {dims:?}");
    assert!(
        signals
            .iter()
            .any(|s| s["dimension"] == "response_preference" && s["value"] == "step_by_step"),
        "explicit step-by-step preference detected: {body}"
    );
    assert!(
        body["messageStats"]["customer_messages"]
            .as_i64()
            .is_some_and(|n| n >= 1),
        "message stats served: {body}"
    );

    // ── 3. GET after refresh: the stored snapshot is served ──────────────
    // THE C7 regression: the old handler's fictional SQL made this route
    // return an empty list forever, no matter what the engine had stored.
    let r = client
        .get(format!("{base}/api/interaction/700"))
        .send()
        .await
        .expect("get after refresh");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    let signals = body["signals"].as_array().cloned().unwrap_or_default();
    assert!(
        !signals.is_empty(),
        "stored snapshot served after refresh: {body}"
    );
    assert!(
        body["generatedAt"].as_str().is_some_and(|g| !g.is_empty()),
        "generatedAt served: {body}"
    );
    assert_eq!(body["provenance"], json!("heuristic"), "body: {body}");
    assert_eq!(
        body["analysisVersion"],
        json!("heuristic_v1"),
        "body: {body}"
    );
    assert!(
        body["messageStats"].is_object(),
        "messageStats served: {body}"
    );

    // ── 4. Evidence: persisted rows with parsed evidence_data ─────────────
    let r = client
        .get(format!("{base}/api/interaction/700/evidence"))
        .send()
        .await
        .expect("evidence");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["conversationId"], json!(700));
    let evidence = body["evidence"].as_array().cloned().unwrap_or_default();
    assert!(
        !evidence.is_empty(),
        "evidence rows persisted by the refresh: {body}"
    );
    for e in &evidence {
        assert!(e["id"].as_i64().is_some(), "row shape: {e}");
        assert_eq!(e["conversation_id"], json!(700), "row shape: {e}");
        let etype = e["evidence_type"].as_str().unwrap_or_default();
        assert!(!etype.is_empty(), "evidence_type is the dimension: {e}");
        // evidence_data serves the parsed object: excerpt + pointers + the
        // signal context.
        assert!(
            e["evidence_data"]["excerpt"]
                .as_str()
                .is_some_and(|x| !x.is_empty()),
            "parsed evidence_data with excerpt: {e}"
        );
        assert!(
            e["evidence_data"]["value"].as_str().is_some(),
            "evidence_data carries the signal value: {e}"
        );
        assert_eq!(e["evidence_data"]["source"], json!("heuristic"), "{e}");
        assert!(
            e["created_at"].as_str().is_some_and(|c| !c.is_empty()),
            "created_at: {e}"
        );
    }
    // The DB has the same rows (real persistence, not response fabrication).
    let db_rows: i64 = {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row(
            "SELECT COUNT(*) FROM interaction_evidence WHERE conversation_id = 700",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    assert_eq!(
        db_rows,
        evidence.len() as i64,
        "HTTP evidence == DB rows (C7: real data, not fabricated)"
    );

    // ── 5. Profile: aggregated from the signals layer ─────────────────────
    let r = client
        .get(format!("{base}/api/interaction/profile/500"))
        .send()
        .await
        .expect("profile");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["customerId"], json!(500));
    assert!(
        body["totalSignals"].as_i64().is_some_and(|n| n >= 5),
        "totalSignals aggregated from real signals: {body}"
    );
    assert_eq!(body["conversationsAnalyzed"], json!(1), "body: {body}");
    let breakdown = body["signalBreakdown"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        breakdown
            .iter()
            .any(|d| d["dimension"] == "frustration" && d["count"].as_i64().is_some_and(|n| n >= 1)),
        "signalBreakdown from real data: {body}"
    );
    // A single observation is below the pattern threshold → no inferred
    // response preference yet.
    assert!(
        body["responsePreference"].is_null(),
        "no inferred preference below the min observation count: {body}"
    );
    assert!(
        body["lastSignalAt"].as_str().is_some_and(|s| !s.is_empty()),
        "lastSignalAt: {body}"
    );

    // ── 6. Override precedence on the profile ─────────────────────────────
    let r = client
        .post(format!("{base}/api/interaction/profile/500/override"))
        .json(&json!({
            "field": "response_preference",
            "value": "concise",
            "reason": "prefers short answers"
        }))
        .send()
        .await
        .expect("set override");
    assert_eq!(r.status().as_u16(), 200, "override saved");
    let r = client
        .get(format!("{base}/api/interaction/profile/500"))
        .send()
        .await
        .expect("profile after override");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    let pref = &body["responsePreference"];
    assert_eq!(pref["value"], json!("concise"), "override wins: {body}");
    assert_eq!(pref["source"], json!("human"), "override wins: {body}");
    assert_eq!(pref["reason"], json!("prefers short answers"), "{body}");
    assert_eq!(
        body["override"]["field"],
        json!("response_preference"),
        "override row served: {body}"
    );

    // ── 7. 404 reference envelopes on unknown ids ─────────────────────────
    for (method, path) in [
        ("GET", "/api/interaction/999999"),
        ("POST", "/api/interaction/999999/refresh"),
        ("GET", "/api/interaction/999999/evidence"),
        ("GET", "/api/interaction/profile/999999"),
    ] {
        let req = if method == "GET" {
            client.get(format!("{base}{path}"))
        } else {
            client.post(format!("{base}{path}"))
        };
        let r = req.send().await.expect("probe");
        let status = r.status().as_u16();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        assert_eq!(status, 404, "{method} {path}: {status} {body}");
        assert_eq!(body["statusCode"], json!(404), "{method} {path}: {body}");
        assert_eq!(body["error"], json!("NotFound"), "{method} {path}: {body}");
        assert!(
            body["message"].as_str().is_some_and(|m| !m.is_empty()),
            "{method} {path}: {body}"
        );
    }

    println!("interaction_routes: all probes passed");
}
