//! Interaction routes (AI-16/AI-17/AI-18): the reference engine's card /
//! two-stage enrichment / observations-evidence / full-profile contract.
//!
//! This suite boots the REAL HTTP server against a seeded database and
//! proves the interactions routes serve the MAIN wire contract
//! (src/server/routes/interactions.ts):
//!
//!   - GET /api/interaction/:id — the full card ({card, labels}): current
//!     signals, baseline, changes, recommendation, outcome, provenance.
//!   - POST /api/interaction/:id/refresh — deterministic recompute + the
//!     two-stage AI enrichment when the backend is enabled (disabled here:
//!     the card stays heuristic-only and ai_enriched is false).
//!   - GET /api/interaction/:id/evidence — the observation rows ({observations}).
//!   - GET /api/interaction/profile/:customerId — the full profile
//!     ({profile}): timeline, preferences, outcomes, playbook, overrides.
//!   - POST/DELETE override — the human response-preference override with
//!     the value-keyed precedence + revert semantics (spec #22, #56).
//!   - 404 reference envelopes for unknown conversations/customers.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PORT: u16 = 3985;

// Multi-thread runtime: the server's async task must keep running while the
// test thread drives the HTTP probes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interaction_routes_serve_the_reference_card_contract() {
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
    // One CLOSED historical conversation (baseline material).
    conn.execute(
        "INSERT INTO conversations
            (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at)
         VALUES (699, 99699, 6999, 'API token expired', 'closed', 1, 500, datetime('now', '-20 days'))",
        [],
    )
    .expect("seed history conversation");
    conn.execute(
        "INSERT INTO conversation_threads
            (conversation_id, type, state, body_text, from_type, created_at)
         VALUES (699, 'customer', 'published',
            'The API returns 401 again. This is STILL not working, very annoying. Please fix ASAP.',
            'customer', datetime('now', '-20 days'))",
        [],
    )
    .expect("seed history thread");
    conn.execute(
        "INSERT INTO conversation_threads
            (conversation_id, type, state, body_text, from_type, created_at)
         VALUES (699, 'reply', 'published', 'Rotated the token for you.',
            'user', datetime('now', '-19 days'))",
        [],
    )
    .expect("seed history reply");
    conn.execute(
        "INSERT INTO conversation_threads
            (conversation_id, type, state, body_text, from_type, created_at)
         VALUES (699, 'customer', 'published', 'Thanks, that worked!',
            'customer', datetime('now', '-19 days'))",
        [],
    )
    .expect("seed closing ack");
    // Conversation 700 (remote 99700) — today's ticket, owned by customer 500.
    conn.execute(
        "INSERT INTO conversations
            (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at)
         VALUES (700, 99700, 7001, 'API returns 401 again', 'active', 1, 500, datetime('now', '-1 day'))",
        [],
    )
    .expect("seed conversation");
    // One customer thread with deterministic markers: frustration ("still",
    // "already", "nobody", "every time"), technical language (api, endpoint,
    // 401, token) and an explicit step-by-step preference.
    conn.execute(
        "INSERT INTO conversation_threads
            (conversation_id, type, state, body_text, from_type, created_at)
         VALUES (700, 'customer', 'published',
            'This is STILL not working. I already called twice and nobody fixed it. Every time the same thing. Please walk me through step by step how to fix the API endpoint, the 401 says the token expired.',
            'customer', datetime('now', '-1 day'))",
        [],
    )
    .expect("seed customer thread");
    // Materialize the history the way workers.onAfterInitialSync does
    // (record + outcome per conversation) so the baseline has membership.
    spp_core::interaction_current::record_current_interaction(&conn, 699)
        .expect("record history snapshot");
    spp_core::interaction_engine::compute_outcome(&conn, 699).expect("compute history outcome");
    // AI disabled: the two-stage enrichment deterministically degrades to the
    // heuristic card (no LM Studio is reachable in the test environment).
    spp_core::settings::set_string(&conn, "ai_enabled", "false").expect("disable AI");

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

    // ── 1. GET the card: full MAIN shape ({card, labels}) ───────────────────
    let r = client
        .get(format!("{base}/api/interaction/700"))
        .send()
        .await
        .expect("get card");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    let labels = &body["labels"];
    assert_eq!(
        labels["featureTitle"],
        json!("Client Interaction Profile"),
        "labels served: {body}"
    );
    let card = &body["card"];
    assert_eq!(card["conversation_local_id"], json!(700), "{card}");
    assert_eq!(card["client_kind"], json!("returning"), "{card}");
    let signals = card["current"]["signals"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !signals.is_empty(),
        "card computes signals on the fly: {card}"
    );
    let dims: Vec<&str> = signals
        .iter()
        .filter_map(|s| s["dimension"].as_str())
        .collect();
    assert!(dims.contains(&"frustration"), "dims: {dims:?}");
    assert!(dims.contains(&"technical_language"), "dims: {dims:?}");
    assert!(dims.contains(&"response_preference"), "dims: {dims:?}");
    // Baseline from the CLOSED history conversation (spec #5: closed-only
    // membership — today's open ticket is current, not normal).
    assert!(card["baseline"].is_object(), "baseline present: {card}");
    assert!(
        card["baseline"]["dimensions"]
            .as_array()
            .is_some_and(|d| !d.is_empty()),
        "baseline dimensions: {card}"
    );
    // The heuristic recommendation is always present (spec #13).
    let rec = &card["recommendation"];
    assert!(rec.is_object(), "heuristic recommendation: {card}");
    assert_eq!(rec["source"], json!("heuristic"), "{rec}");
    assert!(!rec["response_strategy"].as_array().unwrap().is_empty());
    // Outcome fields + provenance.
    assert!(card["effort_score"].is_number(), "{card}");
    assert!(card["friction"].is_string(), "{card}");
    assert_eq!(card["provenance"]["ai_generated"], json!(false), "{card}");
    assert!(card["repeat_issue"].is_object(), "{card}");

    // ── 2. POST refresh: deterministic + graceful AI degradation ──────────
    let r = client
        .post(format!("{base}/api/interaction/700/refresh"))
        .send()
        .await
        .expect("refresh");
    assert_eq!(r.status().as_u16(), 200, "refresh answers 200");
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["ok"], json!(true), "body: {body}");
    assert_eq!(body["ai_enriched"], json!(false), "AI disabled: {body}");
    assert_eq!(body["error"], json!(null), "no degradation error: {body}");
    assert!(body["card"].is_object(), "card served with refresh: {body}");
    let signals = body["card"]["current"]["signals"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(!signals.is_empty(), "refresh persists REAL signals: {body}");
    for s in &signals {
        assert!(s["dimension"].as_str().is_some(), "dimension: {s}");
        assert!(s["value"].as_str().is_some(), "value: {s}");
        assert!(s["confidence"].as_str().is_some(), "confidence: {s}");
        assert_eq!(s["source"], json!("heuristic"), "source: {s}");
    }
    assert!(
        signals
            .iter()
            .any(|s| s["dimension"] == "response_preference" && s["value"] == "step_by_step"),
        "explicit step-by-step preference detected: {body}"
    );
    assert!(
        body["card"]["current"]["message_stats"]["customer_messages"]
            .as_i64()
            .is_some_and(|n| n >= 1),
        "message stats served: {body}"
    );

    // ── 3. GET after refresh: the stored snapshot rides the card ───────────
    let r = client
        .get(format!("{base}/api/interaction/700"))
        .send()
        .await
        .expect("get after refresh");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    let signals = body["card"]["current"]["signals"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !signals.is_empty(),
        "stored snapshot served after refresh: {body}"
    );
    assert!(
        body["card"]["current"]["generated_at"]
            .as_str()
            .is_some_and(|g| !g.is_empty()),
        "generatedAt served: {body}"
    );
    assert_eq!(
        body["card"]["current"]["sources"],
        json!("heuristic"),
        "sources label: {body}"
    );

    // ── 4. Evidence: the observation rows ({observations}) ─────────────────
    let r = client
        .get(format!("{base}/api/interaction/700/evidence"))
        .send()
        .await
        .expect("evidence");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    let observations = body["observations"].as_array().cloned().unwrap_or_default();
    assert!(
        !observations.is_empty(),
        "observations persisted by the refresh: {body}"
    );
    for o in &observations {
        assert!(o["dimension"].as_str().is_some(), "row shape: {o}");
        assert!(o["value"].as_str().is_some(), "row shape: {o}");
        assert_eq!(
            o["conversation_local_id"],
            json!(700),
            "scoped to conv: {o}"
        );
        assert_eq!(o["provenance"], json!("heuristic"), "provenance: {o}");
        assert!(
            o["observed_at"].as_str().is_some_and(|t| !t.is_empty()),
            "observed_at: {o}"
        );
    }
    // The DB has the same rows (real persistence, not response fabrication).
    let db_rows: i64 = {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row(
            "SELECT COUNT(*) FROM client_behavior_observations WHERE conversation_id = 700",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    assert_eq!(
        db_rows,
        observations.len() as i64,
        "HTTP observations == DB rows (real data, not fabricated)"
    );

    // ── 5. Profile: the full MAIN shape ({profile}) ─────────────────────────
    let r = client
        .get(format!("{base}/api/interaction/profile/500"))
        .send()
        .await
        .expect("profile");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    let profile = &body["profile"];
    assert_eq!(profile["customer_local_id"], json!(500), "{profile}");
    assert_eq!(profile["client_kind"], json!("returning"), "{profile}");
    assert!(
        profile["baseline"].is_object(),
        "baseline served: {profile}"
    );
    // Timeline: one month entry per conversation batch.
    assert!(
        profile["timeline"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "timeline served: {profile}"
    );
    // Outcomes: BOTH conversations now have outcomes — the card GET in step 1
    // computed today's ticket's outcome and the fixture computed the history
    // one (total=2; only the closed 699 resolved after first response).
    let outcomes = &profile["outcomes"];
    assert!(outcomes.is_object(), "outcomes served: {profile}");
    assert_eq!(outcomes["total_conversations"], json!(2), "{outcomes}");
    assert_eq!(
        outcomes["first_response_resolution_rate"],
        json!(0.5),
        "{outcomes}"
    );
    // A single step_by_step observation is below the 3-conversation pattern
    // threshold → no inferred preference (spec #39/#40 overfit guard).
    let prefs = profile["preferences"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        prefs
            .iter()
            .all(|p| p["preference"] != json!("step_by_step")),
        "no inferred preference below threshold: {profile}"
    );
    // The playbook derives from the baseline + outcomes.
    assert!(profile["playbook"].is_object(), "playbook: {profile}");

    // ── 6. Override precedence + revert (spec #22, #56) ─────────────────────
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
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["ok"], json!(true), "{body}");
    // The value-keyed preference row carries the human-entered origin.
    let r = client
        .get(format!("{base}/api/interaction/profile/500"))
        .send()
        .await
        .expect("profile after override");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    let prefs = body["profile"]["preferences"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let concise = prefs
        .iter()
        .find(|p| p["preference"] == json!("concise"))
        .expect("materialized preference row");
    assert_eq!(concise["origin"], json!("human_entered"), "{concise}");
    assert_eq!(
        concise["human_override"]["reason"],
        json!("prefers short answers"),
        "{concise}"
    );
    // The active override decision is served on the profile.
    let overrides = body["profile"]["overrides"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        overrides
            .iter()
            .any(|o| o["field"] == json!("response_preference")
                && o["human_value"] == json!("concise")),
        "override row served: {body}"
    );
    // The card's recommendation honors the override (length concise).
    let r = client
        .get(format!("{base}/api/interaction/700"))
        .send()
        .await
        .expect("card after override");
    let body: Value = r.json().await.expect("json");
    assert_eq!(
        body["card"]["recommendation"]["length"],
        json!("concise"),
        "override wins: {body}"
    );
    assert_eq!(
        body["card"]["recommendation"]["source"],
        json!("ai+human-override"),
        "override wins: {body}"
    );
    // Clear → AI semantics restored.
    let r = client
        .delete(format!(
            "{base}/api/interaction/profile/500/override/response_preference"
        ))
        .send()
        .await
        .expect("clear override");
    assert_eq!(r.status().as_u16(), 200, "clear answers 200");
    let r = client
        .get(format!("{base}/api/interaction/profile/500"))
        .send()
        .await
        .expect("profile after clear");
    let body: Value = r.json().await.expect("json");
    let prefs = body["profile"]["preferences"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !prefs.iter().any(|p| p["origin"] == json!("human_entered")),
        "phantom human-entered row removed: {body}"
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
