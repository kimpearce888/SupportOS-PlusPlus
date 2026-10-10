//! SY-09/AI-14/AI-21/AN-04/AN-05 verification: the support-intelligence
//! report batch on the real HTTP server.
//!
//! - POST /api/reports/narrative — the route rides the existing
//!   `ai_pipeline::report_narrative` (startRun -> LM Studio chatJson ->
//!   completeRun): 200 `{ok, narrative, ai_generated, note}` against a
//!   local mock LM Studio, 422 zod-shaped envelopes for bad bodies, and
//!   the run is persisted (type=report_narrative, status=completed).
//! - GET /api/reports/why-contacting — `analyticsService.whyCustomersContact`:
//!   latest completed ticket_analysis per conversation, grouped by
//!   issue_cluster_candidate (lowercased/trimmed) with conversation ids,
//!   sorted by count desc; failed/stale/superseded runs excluded.
//! - GET /api/reports/top-questions — `analyticsService.topQuestions`:
//!   grouped by lowercased/trimmed primary_question, sorted desc, capped
//!   at 20.
//! - GET /api/analytics/ai — delegates to the same implementation as
//!   /api/ai/analytics (identical bodies).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4001;
const LM_PORT: u16 = 4002;

fn seed_run(
    conn: &rusqlite::Connection,
    hash: &str,
    conversation_id: Option<i64>,
    status: &str,
    created_at: &str,
    output: Value,
) {
    conn.execute(
        "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type,
                              conversation_id, status, created_at)
         VALUES (?1, 'v1', 'test-model', ?2, 'ticket_analysis', ?3, ?4, ?5)",
        rusqlite::params![
            hash,
            serde_json::to_string(&output).unwrap(),
            conversation_id,
            status,
            created_at
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn reports_intelligence_batch() {
    // ── Local mock LM Studio (OpenAI-compatible) ───────────────────────────
    async fn lm_chat() -> axum::Json<Value> {
        axum::Json(json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "{\"narrative\": \"Ticket volume is stable and ratings are trending up.\"}"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30 }
        }))
    }
    let lm_app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(lm_chat));
    let lm_listener = tokio::net::TcpListener::bind(("127.0.0.1", LM_PORT))
        .await
        .expect("mock LM bind");
    tokio::spawn(async move {
        axum::serve(lm_listener, lm_app)
            .await
            .expect("mock LM server");
    });

    // ── Boot the real server with the mock as the AI backend ─────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");
    spp_core::ai_pipeline::ensure_pipeline_schema(&conn).expect("pipeline schema");

    // LM Studio base URL pointing at the mock (URL normalization appends /v1).
    spp_core::settings::set_string(
        &conn,
        "lmstudio_base_url",
        &format!("http://127.0.0.1:{LM_PORT}"),
    )
    .expect("set lmstudio_base_url");

    // ── Seed conversations + analyses ──────────────────────────────────────
    conn.execute(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
        [],
    )
    .unwrap();
    // DB-03: M047 FKs (foreign_keys=ON) — customer 100 must exist.
    conn.execute(
        "INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (100, 2100, 'Eve')",
        [],
    )
    .unwrap();
    let insert_conv = |remote: i64| {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, mailbox_local_id, customer_local_id, status, created_at, updated_at)
             VALUES (?1, ?1, 1, 100, 'closed', datetime('now'), datetime('now'))",
            rusqlite::params![remote],
        )
        .unwrap();
    };
    for remote in 1..=4i64 {
        insert_conv(remote);
    }
    let conv_id = |remote: i64| -> i64 {
        conn.query_row(
            "SELECT id FROM conversations WHERE remote_id = ?1",
            rusqlite::params![remote],
            |r| r.get(0),
        )
        .unwrap()
    };
    let (c1, c2, c3, c4) = (conv_id(1), conv_id(2), conv_id(3), conv_id(4));

    // Fresh-timestamp seeds (start_run writes datetime('now') format).
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    // 1. c1 run, later SUPERSEDED by run 2 (only the latest per
    //    conversation may count).
    seed_run(
        &conn,
        "h1",
        Some(c1),
        "completed",
        &now,
        json!({ "issue_cluster_candidate": "login trouble", "primary_question": "how do I log in?" }),
    );
    // 2. c1 latest: "Billing" (uppercase mixes with c2's "BILLING").
    seed_run(
        &conn,
        "h2",
        Some(c1),
        "completed",
        &now,
        json!({
            "issue_cluster_candidate": "Billing",
            "primary_question": "How do I UPDATE my card? "
        }),
    );
    // 3. c2: same category + same question after lowercase/trim.
    seed_run(
        &conn,
        "h3",
        Some(c2),
        "completed",
        &now,
        json!({ "issue_cluster_candidate": "BILLING", "primary_question": "how do I update my card?" }),
    );
    // 4. c3: a different category + question.
    seed_run(
        &conn,
        "h4",
        Some(c3),
        "completed",
        &now,
        json!({ "issue_cluster_candidate": "shipping delay", "primary_question": "Where is my order?" }),
    );
    // 5. c2 failed run: never counted anywhere.
    seed_run(
        &conn,
        "h5",
        Some(c2),
        "failed",
        &now,
        json!({ "issue_cluster_candidate": "never counted", "primary_question": "never" }),
    );
    // 6. c3 stale run, inserted AFTER h4 — it is the LATEST completed run
    //    for c3, so the day window excludes c3 entirely (reference
    //    semantics: latest-run-per-conversation THEN days filter).
    seed_run(
        &conn,
        "h6",
        Some(c3),
        "completed",
        "2020-01-01 09:00:00",
        json!({ "issue_cluster_candidate": "ancient", "primary_question": "ancient question" }),
    );
    // 7. c4 stale run inserted BEFORE its fresh run — superseded, ignored.
    seed_run(
        &conn,
        "h7",
        Some(c4),
        "completed",
        "2020-01-01 09:00:00",
        json!({ "issue_cluster_candidate": "ancient", "primary_question": "ancient question" }),
    );
    // 8. c4 fresh latest run.
    seed_run(
        &conn,
        "h8",
        Some(c4),
        "completed",
        &now,
        json!({ "issue_cluster_candidate": "shipping delay", "primary_question": "Where is my order?" }),
    );

    // Extra conversations for the top-20 cap (remote 10..=30: 21 more).
    for remote in 10..=30i64 {
        insert_conv(remote);
        let id = conv_id(remote);
        seed_run(
            &conn,
            &format!("hx{remote}"),
            Some(id),
            "completed",
            &now,
            json!({
                "issue_cluster_candidate": "misc",
                "primary_question": format!("Question number {remote}?")
            }),
        );
    }
    // One more failed analysis (for the analytics success rate).
    seed_run(&conn, "hf", Some(c1), "failed", &now, json!({}));

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
        data_dir,
        port: SERVER_PORT,
        host: "127.0.0.1".to_string(),
        demo_mode: false,
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

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{SERVER_PORT}");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(r) = client.get(format!("{base}/health")).send().await {
            if r.status().as_u16() == 200 {
                break;
            }
        }
        assert!(Instant::now() < deadline, "server did not come up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // ── GET /api/reports/why-contacting ──────────────────────────────────
    let resp = client
        .get(format!("{base}/api/reports/why-contacting"))
        .send()
        .await
        .expect("why-contacting");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["source"], json!("ai-derived"));
    let cats = body["categories"].as_array().expect("categories");
    // Latest-per-conversation then day-window: c1 -> "billing" (run 2
    // supersedes run 1), c2 -> "billing", c3 -> its latest run is the
    // stale one so c3 drops out, c4 -> "shipping delay" (fresh run
    // supersedes its stale one), + the 21 "misc" extras. Sorted by
    // count desc: misc(21) > billing(2) > shipping delay(1).
    assert_eq!(cats.len(), 3, "categories: {cats:?}");
    assert_eq!(cats[0]["category"], json!("misc"));
    assert_eq!(cats[0]["count"], json!(21));
    assert_eq!(cats[1]["category"], json!("billing"));
    assert_eq!(cats[1]["count"], json!(2));
    assert_eq!(cats[1]["conversation_ids"], json!([c1, c2]));
    assert_eq!(cats[2]["category"], json!("shipping delay"));
    assert_eq!(cats[2]["count"], json!(1));
    assert_eq!(cats[2]["conversation_ids"], json!([c4]));
    let names: Vec<&str> = cats
        .iter()
        .map(|c| c["category"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"login trouble"), "superseded: {names:?}");
    assert!(!names.contains(&"never counted"), "failed: {names:?}");
    assert!(!names.contains(&"ancient"), "stale: {names:?}");

    // ── GET /api/reports/top-questions ────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/reports/top-questions"))
        .send()
        .await
        .expect("top-questions");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("json");
    let questions = body["questions"].as_array().expect("questions");
    // Distinct questions: 1 grouped (count 2) + 22 singles (c3 + 21
    // extras) = 23 -> capped at 20.
    assert_eq!(questions.len(), 20, "cap at 20: len = {}", questions.len());
    // The grouped duplicate (count 2) sorts first.
    assert_eq!(questions[0]["question"], json!("how do i update my card?"));
    assert_eq!(questions[0]["count"], json!(2));
    assert_eq!(questions[0]["conversation_ids"], json!([c1, c2]));
    // No stale question, no failed question.
    let texts: Vec<&str> = questions
        .iter()
        .map(|q| q["question"].as_str().unwrap())
        .collect();
    assert!(!texts.contains(&"ancient question"));
    assert!(!texts.contains(&"never"));

    // ── GET /api/analytics/ai == GET /api/ai/analytics ────────────────────
    let a = client
        .get(format!("{base}/api/analytics/ai"))
        .send()
        .await
        .expect("analytics/ai");
    assert_eq!(a.status().as_u16(), 200);
    let a: Value = a.json().await.expect("json");
    let b = client
        .get(format!("{base}/api/ai/analytics"))
        .send()
        .await
        .expect("ai/analytics");
    assert_eq!(b.status().as_u16(), 200);
    let b: Value = b.json().await.expect("json");
    assert_eq!(a, b, "/api/analytics/ai must serve /api/ai/analytics");
    assert_eq!(a["source"], json!("local"));
    // 28 completed runs (h1-h4, h6, h7, h8 = 7, + 21 extras) over 25
    // distinct conversations (c1..c4 + 21 extras), + 2 failed (h5, hf)
    // -> 28/30 = 93.3%.
    assert_eq!(a["tickets_analyzed"], json!(25), "body: {a}");
    assert_eq!(a["analysis_success_rate"], json!(93.3));

    // ── POST /api/reports/narrative — happy path against the mock ─────────
    let resp = client
        .post(format!("{base}/api/reports/narrative"))
        .json(&json!({
            "reportName": "Weekly Overview",
            "facts": { "tickets": 42, "trend": "up", "beta": true, "note": null }
        }))
        .send()
        .await
        .expect("narrative ok");
    assert_eq!(resp.status().as_u16(), 200, "must be 200");
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(
        body["narrative"],
        json!("Ticket volume is stable and ratings are trending up.")
    );
    assert_eq!(body["ai_generated"], json!(true));
    assert_eq!(
        body["note"],
        json!("This narrative was AI-generated locally from the computed facts above.")
    );
    // The run is recorded: type=report_narrative, status=completed, output
    // persisted with the narrative (reference completeRun({narrative})).
    {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let (status, output): (String, String) = c
            .query_row(
                "SELECT status, response_json FROM ai_runs WHERE type = 'report_narrative'
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("report_narrative run persisted");
        assert_eq!(status, "completed");
        let output: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(output["narrative"], body["narrative"]);
    }

    // ── POST /api/reports/narrative — zod 422 envelopes ───────────────────
    let resp = client
        .post(format!("{base}/api/reports/narrative"))
        .json(&json!({ "facts": {} }))
        .send()
        .await
        .expect("missing reportName");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["statusCode"], json!(422));
    assert_eq!(body["error"], json!("ValidationError"));
    assert_eq!(
        body["message"],
        json!("Invalid request (reportName): Required")
    );
    assert_eq!(
        body["issues"],
        json!([{ "path": "reportName", "message": "Required" }])
    );

    let resp = client
        .post(format!("{base}/api/reports/narrative"))
        .json(&json!({ "reportName": "" }))
        .send()
        .await
        .expect("empty reportName");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["message"],
        json!("Invalid request (reportName): String must contain at least 1 character(s)")
    );

    let resp = client
        .post(format!("{base}/api/reports/narrative"))
        .json(&json!({ "reportName": "x".repeat(201) }))
        .send()
        .await
        .expect("too long reportName");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["message"],
        json!("Invalid request (reportName): String must contain at most 200 character(s)")
    );

    let resp = client
        .post(format!("{base}/api/reports/narrative"))
        .json(&json!({ "reportName": "R", "facts": { "bad": [1, 2] } }))
        .send()
        .await
        .expect("facts array value");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["message"],
        json!("Invalid request (facts.bad): Expected string, received array")
    );

    let resp = client
        .post(format!("{base}/api/reports/narrative"))
        .json(&json!({ "reportName": "R", "facts": "nope" }))
        .send()
        .await
        .expect("facts non-object");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["message"],
        json!("Invalid request (facts): Expected object, received string")
    );

    // Missing facts is fine (zod default {}): the mock still serves it.
    let resp = client
        .post(format!("{base}/api/reports/narrative"))
        .json(&json!({ "reportName": "NoFacts" }))
        .send()
        .await
        .expect("no facts");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["ok"], json!(true));
}
