//! IS-03 verification: engineering refs + support cases on the real HTTP
//! server (reference routes/issues.ts:109-147 + issueRepo.ts:231-281).
//!
//! - POST /api/issues/known/:id/refs — the zod schema message-for-message
//!   (system/reference_id required bounded strings, url/title/status/notes
//!   optional bounded strings — none nullable), unknown keys stripped,
//!   the `{ok, message}` answer, the FK 500 envelope on unknown issue ids,
//!   and the stored row served back through GET /api/issues/known/:id
//!   `engineering_refs`.
//! - GET /api/issues/cases — the reference `SupportCaseRecord` wire shape
//!   (all 16 columns, tags parsed from JSON storage to the served array),
//!   newest-first; a malformed tags row fails honestly with the 500
//!   envelope (the reference's `JSON.parse` throw).
//! - POST /api/issues/cases/from-conversation/:conversationId — the
//!   200 `{ok: false}` on unknown conversations; the capture composes
//!   problem (analysis customer_goal ?? subject), root_question
//!   (primary_question), resolution/answer (first 2000 chars of the last
//!   published reply — drafts skipped), product/feature (analysis), tag
//!   names, agent and rating; and the recapture upsert overwrites every
//!   column, including clearing resolution_time_min (reference behavior).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4017;

#[tokio::test]
async fn issues_refs_cases_parity() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // A small mirror world: one customer/agent, three conversations
    // (the full-capture one, the bare-fallback one, the recapture one),
    // a known issue to hang refs on, one pre-seeded support case, tags,
    // a rating and a completed ticket analysis for conversation 1.
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO users (id, remote_id, first_name, last_name) VALUES (7, 70, 'Dana', 'Reyes');
         INSERT INTO customers (id, remote_id, first_name, email) VALUES (11, 110, 'Ada', 'ada@example.com');
         INSERT INTO customers (id, remote_id, first_name, email) VALUES (12, 120, 'Belle', 'belle@example.com');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id, created_at)
             VALUES (1, 101, 101, 'Export stuck at night', 'closed', 1, 11, 7, '2026-10-01 10:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at)
             VALUES (2, 102, 102, 'Bare question', 'active', 1, 12, '2026-10-02 10:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at, remote_created_at)
             VALUES (3, 103, 103, 'Timezone drift', 'active', 1, 12, '2026-10-03 10:00:00', '2026-10-03 09:00:00');
         INSERT INTO known_issues (id, name, status, description)
             VALUES (90, 'Legacy login bug', 'active', 'pre-IS-02 row');
         INSERT INTO tags (id, remote_id, name) VALUES (21, 210, 'billing');
         INSERT INTO tags (id, remote_id, name) VALUES (22, 220, 'export');
         INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (1, 21), (1, 22);
         INSERT INTO ratings (conversation_id, rating, remote_created_at)
             VALUES (1, 'great', '2026-10-01 12:00:00');
         INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at, state)
             VALUES (1, 'reply', 'Older published reply', 'user', '2026-10-01 11:00:00', 'published');
         INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at, state)
             VALUES (1, 'note', 'internal note body', 'user', '2026-10-01 11:30:00', 'published');
         INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at, state)
             VALUES (1, 'reply', 'DRAFT never served', 'user', '2026-10-01 11:45:00', 'draft');
         INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type, conversation_id, status, created_at)
             VALUES ('is03-hash', 'v1', 'test-model',
                     '{\"customer_goal\": \"Fix the nightly export schedule\", \"primary_question\": \"Why does the export stall after midnight?\", \"product\": \"Reports\", \"feature\": \"Schedules\"}',
                     'ticket_analysis', 1, 'completed', '2026-10-01 12:00:00');
         INSERT INTO support_cases (conversation_id, customer_id, problem, root_question, resolution, answer,
                                    product, feature, tags, fields, agent_user_id, resolution_time_min, rating, created_at)
             VALUES (3, 12, 'Pre-seeded problem', 'Pre-seeded question', 'Pre-seeded resolution', 'Pre-seeded answer',
                     'Reports', 'Timezones', '[\"timezone\"]', NULL, NULL, 55.5, 'okay', '2026-10-04 09:00:00');",
    )
    .expect("seed world");

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

    let support_case = |conversation_id: i64| -> Option<Value> {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT id, conversation_id, customer_id, problem, root_question, resolution, answer,
                    product, feature, tags, agent_user_id, resolution_time_min, rating
             FROM support_cases WHERE conversation_id = ?1",
            rusqlite::params![conversation_id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "conversation_id": r.get::<_, Option<i64>>(1)?,
                    "customer_id": r.get::<_, Option<i64>>(2)?,
                    "problem": r.get::<_, Option<String>>(3)?,
                    "root_question": r.get::<_, Option<String>>(4)?,
                    "resolution": r.get::<_, Option<String>>(5)?,
                    "answer": r.get::<_, Option<String>>(6)?,
                    "product": r.get::<_, Option<String>>(7)?,
                    "feature": r.get::<_, Option<String>>(8)?,
                    "tags": r.get::<_, Option<String>>(9)?,
                    "agent_user_id": r.get::<_, Option<i64>>(10)?,
                    "resolution_time_min": r.get::<_, Option<f64>>(11)?,
                    "rating": r.get::<_, Option<String>>(12)?,
                }))
            },
        )
        .ok()
    };

    // ── POST /api/issues/known/:id/refs — the happy path ───────────────
    let r = client
        .post(format!("{base}/api/issues/known/90/refs"))
        .json(&json!({
            "system": "linear",
            "reference_id": "SUP-1234",
            "url": "https://linear.app/issue/SUP-1234",
            "title": "Nightly export stalls",
            "status": "in progress",
            "notes": "Reproduced on staging"
        }))
        .send()
        .await
        .expect("add ref");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body,
        json!({"ok": true, "message": "Engineering reference added."})
    );

    // The stored row serves back through the known-issue detail.
    let r = client
        .get(format!("{base}/api/issues/known/90"))
        .send()
        .await
        .expect("known detail");
    let body: Value = r.json().await.expect("body");
    let refs = body["known_issue"]["engineering_refs"]
        .as_array()
        .expect("refs");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["known_issue_id"], json!(90));
    assert_eq!(refs[0]["system"], json!("linear"));
    assert_eq!(refs[0]["reference_id"], json!("SUP-1234"));
    assert_eq!(refs[0]["url"], json!("https://linear.app/issue/SUP-1234"));
    assert_eq!(refs[0]["title"], json!("Nightly export stalls"));
    assert_eq!(refs[0]["status"], json!("in progress"));
    assert_eq!(refs[0]["notes"], json!("Reproduced on staging"));
    assert!(refs[0]["id"].is_i64());

    // A second ref stacks (rowid order).
    client
        .post(format!("{base}/api/issues/known/90/refs"))
        .json(&json!({"system": "github", "reference_id": "#42"}))
        .send()
        .await
        .expect("add ref 2");
    let body: Value = client
        .get(format!("{base}/api/issues/known/90"))
        .send()
        .await
        .expect("known detail")
        .json()
        .await
        .expect("body");
    assert_eq!(
        body["known_issue"]["engineering_refs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // ── refs zod parity ────────────────────────────────────────────────
    async fn probe(client: &reqwest::Client, base: &str, payload: Value) -> (u16, Value) {
        let r = client
            .post(format!("{base}/api/issues/known/90/refs"))
            .json(&payload)
            .send()
            .await
            .expect("probe");
        let status = r.status().as_u16();
        (status, r.json().await.expect("body"))
    }

    // Both requireds absent.
    let (status, body) = probe(&client, &base, json!({})).await;
    assert_eq!(status, 422);
    let issues = body["issues"].as_array().expect("issues");
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0]["path"], json!("system"));
    assert_eq!(issues[0]["message"], json!("Required"));
    assert_eq!(issues[1]["path"], json!("reference_id"));
    assert_eq!(issues[1]["message"], json!("Required"));

    // Wrong types, collected in schema order.
    let (status, body) = probe(&client, &base, json!({"system": 123, "reference_id": null})).await;
    assert_eq!(status, 422);
    let issues = body["issues"].as_array().expect("issues");
    assert_eq!(issues.len(), 2);
    assert_eq!(
        issues[0]["message"],
        json!("Expected string, received number")
    );
    assert_eq!(
        issues[1]["message"],
        json!("Expected string, received null")
    );

    // Empty system (min 1) and over-long reference_id (max 200).
    let (status, body) = probe(
        &client,
        &base,
        json!({"system": "", "reference_id": "x".repeat(201)}),
    )
    .await;
    assert_eq!(status, 422);
    let issues = body["issues"].as_array().expect("issues");
    assert_eq!(issues.len(), 2);
    assert_eq!(
        issues[0]["message"],
        json!("String must contain at least 1 character(s)")
    );
    assert_eq!(
        issues[1]["message"],
        json!("String must contain at most 200 character(s)")
    );

    // The optionals are NOT nullable (unlike the known-issue POST body).
    let (status, body) = probe(
        &client,
        &base,
        json!({"system": "linear", "reference_id": "SUP-1", "url": null}),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(body["issues"][0]["path"], json!("url"));
    assert_eq!(
        body["issues"][0]["message"],
        json!("Expected string, received null")
    );

    // Over-long url (max 2000).
    let (status, body) = probe(
        &client,
        &base,
        json!({"system": "linear", "reference_id": "SUP-1", "url": "x".repeat(2001)}),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(body["issues"][0]["path"], json!("url"));
    assert_eq!(
        body["issues"][0]["message"],
        json!("String must contain at most 2000 character(s)")
    );

    // Unknown keys are stripped (this one succeeds).
    let (status, body) = probe(
        &client,
        &base,
        json!({"system": "linear", "reference_id": "SUP-9", "bogus": true}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], json!(true));

    // Unknown known-issue id: the FK 500 envelope (the reference's FK
    // failure path).
    let r = client
        .post(format!("{base}/api/issues/known/999999/refs"))
        .json(&json!({"system": "linear", "reference_id": "SUP-1"}))
        .send()
        .await
        .expect("fk probe");
    assert_eq!(r.status().as_u16(), 500);
    let body: Value = r.json().await.expect("body");
    assert_eq!(body["statusCode"], json!(500));
    assert_eq!(body["error"], json!("InternalError"));

    // ── GET /api/issues/cases — the seeded row ──────────────────────────
    let r = client
        .get(format!("{base}/api/issues/cases"))
        .send()
        .await
        .expect("list cases");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    let cases = body["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    // The full 16-key reference wire set.
    let mut served: Vec<&str> = case
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    served.sort();
    let mut expected = vec![
        "agent_user_id",
        "answer",
        "conversation_id",
        "created_at",
        "customer_id",
        "feature",
        "fields",
        "id",
        "problem",
        "product",
        "provenance",
        "rating",
        "resolution",
        "resolution_time_min",
        "root_question",
        "tags",
    ];
    expected.sort();
    assert_eq!(served, expected);
    assert_eq!(case["tags"], json!(["timezone"]));
    assert_eq!(case["resolution_time_min"], json!(55.5));
    assert_eq!(case["rating"], json!("okay"));
    assert_eq!(case["problem"], json!("Pre-seeded problem"));

    // ── POST /api/issues/cases/from-conversation — unknown id ──────────
    let r = client
        .post(format!("{base}/api/issues/cases/from-conversation/999999"))
        .send()
        .await
        .expect("unknown conversation");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body,
        json!({"ok": false, "message": "Conversation not found."})
    );

    // ── the full capture (analysis + reply + tags + rating) ────────────
    // A 2500-char newest published reply: resolution/answer carry the first
    // 2000 chars only.
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at, state)
             VALUES (1, 'reply', ?1, 'user', '2026-10-01 12:30:00', 'published')",
            rusqlite::params![format!("{}TAIL", "R".repeat(2495))],
        )
        .expect("long reply");
    }
    let r = client
        .post(format!("{base}/api/issues/cases/from-conversation/1"))
        .send()
        .await
        .expect("capture");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body,
        json!({"ok": true, "message": "Support case captured from this conversation."})
    );

    let case = support_case(1).expect("captured row");
    assert_eq!(case["customer_id"], json!(11));
    assert_eq!(case["problem"], json!("Fix the nightly export schedule"));
    assert_eq!(
        case["root_question"],
        json!("Why does the export stall after midnight?")
    );
    let resolution = case["resolution"].as_str().expect("resolution");
    assert_eq!(resolution.chars().count(), 2000);
    assert!(resolution.starts_with(&"R".repeat(1990)));
    assert!(!resolution.contains("TAIL"));
    assert_eq!(case["resolution"], case["answer"]);
    assert_eq!(case["product"], json!("Reports"));
    assert_eq!(case["feature"], json!("Schedules"));
    assert_eq!(case["tags"], json!("[\"billing\",\"export\"]"));
    assert_eq!(case["agent_user_id"], json!(7));
    assert_eq!(case["rating"], json!("great"));
    assert!(case["resolution_time_min"].is_null());

    // The list now serves both cases (newest-first: the capture just ran).
    let body: Value = client
        .get(format!("{base}/api/issues/cases"))
        .send()
        .await
        .expect("list cases")
        .json()
        .await
        .expect("body");
    let cases = body["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0]["tags"], json!(["billing", "export"]));
    assert_eq!(cases[1]["tags"], json!(["timezone"]));

    // ── the bare fallback (no analysis, no reply, no tags, no rating) ──
    let r = client
        .post(format!("{base}/api/issues/cases/from-conversation/2"))
        .send()
        .await
        .expect("bare capture");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    assert_eq!(body["ok"], json!(true));
    let case = support_case(2).expect("bare row");
    assert_eq!(case["problem"], json!("Bare question"));
    assert!(case["root_question"].is_null());
    assert!(case["resolution"].is_null());
    assert!(case["answer"].is_null());
    assert!(case["product"].is_null());
    assert!(case["feature"].is_null());
    assert_eq!(case["tags"], json!("[]"));
    assert!(case["agent_user_id"].is_null());
    assert!(case["rating"].is_null());
    assert!(case["resolution_time_min"].is_null());

    // ── the recapture upsert overwrites (incl. clearing rtm) ───────────
    // Conversation 3 re-captures over the pre-seeded row: the subject
    // becomes the problem and the pre-set resolution_time_min clears.
    let r = client
        .post(format!("{base}/api/issues/cases/from-conversation/3"))
        .send()
        .await
        .expect("recapture");
    assert_eq!(r.status().as_u16(), 200);
    let cases: Vec<Value> = {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = conn
            .prepare("SELECT conversation_id, problem, resolution_time_min FROM support_cases ORDER BY id")
            .expect("select cases");
        stmt.query_map([], |r| {
            Ok(json!({
                "conversation_id": r.get::<_, i64>(0)?,
                "problem": r.get::<_, Option<String>>(1)?,
                "resolution_time_min": r.get::<_, Option<f64>>(2)?,
            }))
        })
        .expect("query")
        .filter_map(|r| r.ok())
        .collect()
    };
    // Three rows total, one per conversation (the upsert did not add a
    // fourth for conversation 3).
    assert_eq!(cases.len(), 3);
    let recaptured = cases
        .iter()
        .find(|c| c["conversation_id"] == json!(3))
        .expect("conv 3");
    assert_eq!(recaptured["problem"], json!("Timezone drift"));
    assert!(recaptured["resolution_time_min"].is_null());

    // ── malformed tags row fails honestly (reference JSON.parse throw) ─
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO support_cases (conversation_id, customer_id, tags, created_at)
             VALUES (2, 12, 'not-json', '2026-10-05 10:00:00') ON CONFLICT(conversation_id) DO NOTHING",
            [],
        )
        .ok();
        // ON CONFLICT DO NOTHING keeps the valid row; corrupt it directly
        // so the serving path meets the malformed value.
        conn.execute(
            "UPDATE support_cases SET tags = 'not-json' WHERE conversation_id = 2",
            [],
        )
        .expect("corrupt tags");
    }
    let r = client
        .get(format!("{base}/api/issues/cases"))
        .send()
        .await
        .expect("list cases with bad row");
    assert_eq!(r.status().as_u16(), 500);
    let body: Value = r.json().await.expect("body");
    assert_eq!(body["statusCode"], json!(500));
    assert_eq!(body["error"], json!("InternalError"));
}
