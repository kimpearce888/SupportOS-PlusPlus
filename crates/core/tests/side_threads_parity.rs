//! Audit item CL-02 / blocker B2 + N1: side-threads list + detail.
//!
//! B2: the list route's existence check matched `conversations.remote_id`
//! against a local-id path param, so every caller (the UI) got 404 for real
//! conversations — the feature was dead end-to-end. The reference
//! (routes/collaboration.ts:31-43) matches the LOCAL id with the
//! soft-delete filter (`WHERE id = ? AND deleted_at IS NULL`).
//!
//! N1: the detail route returned `{ messages: […] }` (never 404, no
//! thread fields); the reference (routes/collaboration.ts:75-87 +
//! sideThreadRepo.ts:51-104) serves `{ side_thread: … }` with the full
//! payload: summary fields (title/status/message_count/team_name/
//! conversation_number/last_message_at …) + participants + messages with
//! resolved mentions.
//!
//! This test boots the REAL HTTP server (like tests/cors_origins.rs),
//! seeds fixture rows through the real bootstrap schema, and live-probes:
//!
//!   - the list route answers on the LOCAL conversation id (B2) with the
//!     reference payload fields, open threads first, newest update first;
//!   - a REMOTE id (the old bug's id space) and an unknown id 404;
//!   - a soft-deleted conversation 404s (reference deleted_at filter);
//!   - id 0 is a 422 (reference positive-integer validation);
//!   - the detail route serves `{ side_thread: … }` with participants,
//!     messages (author names via users join) and resolved mentions, and
//!     404s for unknown threads.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

const PORT: u16 = 3988;

// Multi-thread runtime: the server's async task must keep running while the
// test drives it from the test thread (same rationale as cors_origins.rs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn side_threads_list_and_detail_match_the_reference_contract() {
    // ── Boot, mirroring the Tauri shell (app/src-tauri/src/lib.rs) ──────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // ── Fixtures: a conversation, users, a team and two threads ─────────────
    conn.execute_batch(
        "INSERT INTO users (id, remote_id, first_name, last_name, mention, email) VALUES
             (1, 501, 'Ada',  'Lovelace', 'ada',    'ada@example.com'),
             (2, 502, 'Grace', 'Hopper',  'grace', 'grace@example.com');
         INSERT INTO teams (id, remote_id, name) VALUES (7, 71, 'Support');
         INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id, status)
             VALUES (9, 100001, 4242, 'Refund question', 1, 1, 'active');
         INSERT INTO side_threads (conversation_id, created_by_user_id, title, team_local_id, status, updated_at)
             VALUES (9, 1, 'Refund question', 7, 'open', '2026-10-05 10:00:00');
         INSERT INTO side_threads (conversation_id, created_by_user_id, title, team_local_id, status, updated_at)
             VALUES (9, 2, 'Old escalation', NULL, 'resolved', '2026-10-04 09:00:00');
         INSERT INTO side_thread_participants (side_thread_id, user_local_id, added_by_user_local_id)
             VALUES ((SELECT id FROM side_threads WHERE title = 'Refund question'), 2, 1);
         INSERT INTO side_thread_messages (thread_id, body, author_user_id, created_at) VALUES
             ((SELECT id FROM side_threads WHERE title = 'Refund question'),
              'First — heads up @grace', 1, '2026-10-05 10:01:00'),
             ((SELECT id FROM side_threads WHERE title = 'Refund question'),
              'Second message', 1, '2026-10-05 10:02:00');
         INSERT INTO side_thread_mentions (side_thread_id, message_id, user_local_id, team_local_id)
             VALUES ((SELECT id FROM side_threads WHERE title = 'Refund question'),
                     (SELECT MIN(id) FROM side_thread_messages), 2, NULL);",
    )
    .expect("seed fixtures");
    let open_thread_id: i64 = conn
        .query_row(
            "SELECT id FROM side_threads WHERE title = 'Refund question'",
            [],
            |r| r.get(0),
        )
        .expect("open thread id");

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

    // ── 1. List on the LOCAL id answers with the reference payload (B2) ────
    let r = client
        .get(format!("{base}/api/conversations/9/side-threads"))
        .send()
        .await
        .expect("list side threads");
    assert_eq!(r.status().as_u16(), 200, "local id must resolve (B2 fix)");
    let body: Value = r.json().await.expect("list body");
    let threads = body
        .get("side_threads")
        .and_then(|v| v.as_array())
        .expect("side_threads array");
    assert_eq!(threads.len(), 2, "two seeded threads: {threads:#?}");

    let first = &threads[0];
    assert_eq!(first["id"].as_i64(), Some(open_thread_id));
    assert_eq!(first["title"].as_str(), Some("Refund question"));
    assert_eq!(first["status"].as_str(), Some("open"));
    assert_eq!(first["message_count"].as_i64(), Some(2));
    assert_eq!(first["team_local_id"].as_i64(), Some(7));
    assert_eq!(first["team_name"].as_str(), Some("Support"));
    assert_eq!(first["conversation_id"].as_i64(), Some(9));
    assert_eq!(first["conversation_number"].as_i64(), Some(4242));
    assert_eq!(
        first["created_by_user_local_id"].as_i64(),
        Some(1),
        "actor served under the reference payload name"
    );
    assert!(
        first["last_message_at"].as_str().is_some(),
        "last_message_at must be served (N1)"
    );
    assert!(first["resolved_at"].is_null());
    assert!(
        first.get("created_by_user_id").is_none(),
        "the port-side column name must not leak into the payload"
    );

    // Open threads first, then resolved (reference ORDER BY status='resolved').
    let second = &threads[1];
    assert_eq!(second["status"].as_str(), Some("resolved"));
    assert_eq!(second["title"].as_str(), Some("Old escalation"));
    assert_eq!(second["message_count"].as_i64(), Some(0));
    assert!(second["team_name"].is_null());
    assert!(second["last_message_at"].is_null());

    // ── 2. The REMOTE id (the old bug's id space) must NOT resolve ─────────
    let r = client
        .get(format!("{base}/api/conversations/100001/side-threads"))
        .send()
        .await
        .expect("remote id probe");
    assert_eq!(
        r.status().as_u16(),
        404,
        "remote_id must not match the local-id route (B2 regression)"
    );

    // ── 3. Unknown conversation 404s with the reference envelope ───────────
    let r = client
        .get(format!("{base}/api/conversations/999999/side-threads"))
        .send()
        .await
        .expect("unknown conversation probe");
    assert_eq!(r.status().as_u16(), 404);
    let body: Value = r.json().await.expect("404 body");
    assert_eq!(body["statusCode"].as_i64(), Some(404));
    assert_eq!(body["error"].as_str(), Some("NotFound"));
    assert_eq!(
        body["message"].as_str(),
        Some("Conversation not found."),
        "reference 404 message"
    );

    // ── 4. id 0 is a 422 (reference positive-integer validation) ───────────
    let r = client
        .get(format!("{base}/api/conversations/0/side-threads"))
        .send()
        .await
        .expect("zero id probe");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("422 body");
    assert_eq!(body["statusCode"].as_i64(), Some(422));
    assert_eq!(
        body["message"].as_str(),
        Some("id must be a positive integer.")
    );

    // ── 5. Detail: { side_thread: … } with participants + messages (N1) ────
    let r = client
        .get(format!("{base}/api/side-threads/{open_thread_id}"))
        .send()
        .await
        .expect("side thread detail");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("detail body");
    let t = body
        .get("side_thread")
        .expect("detail must use the side_thread key (N1)");
    assert_eq!(t["title"].as_str(), Some("Refund question"));
    assert_eq!(t["status"].as_str(), Some("open"));
    assert_eq!(t["message_count"].as_i64(), Some(2));
    assert_eq!(t["team_name"].as_str(), Some("Support"));
    assert_eq!(t["conversation_number"].as_i64(), Some(4242));

    let participants = t
        .get("participants")
        .and_then(|v| v.as_array())
        .expect("participants array");
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0]["user_local_id"].as_i64(), Some(2));
    assert_eq!(participants[0]["first_name"].as_str(), Some("Grace"));
    assert_eq!(participants[0]["last_name"].as_str(), Some("Hopper"));
    assert_eq!(participants[0]["mention"].as_str(), Some("grace"));
    assert_eq!(participants[0]["added_by_user_local_id"].as_i64(), Some(1));

    let messages = t
        .get("messages")
        .and_then(|v| v.as_array())
        .expect("messages array");
    assert_eq!(messages.len(), 2, "oldest-first order");
    assert!(messages[0]["body"]
        .as_str()
        .expect("body")
        .starts_with("First"));
    assert_eq!(messages[0]["side_thread_id"].as_i64(), Some(open_thread_id));
    assert_eq!(messages[0]["author_user_local_id"].as_i64(), Some(1));
    assert_eq!(messages[0]["author_first_name"].as_str(), Some("Ada"));
    assert_eq!(messages[0]["author_last_name"].as_str(), Some("Lovelace"));
    let mentions = messages[0]["mentions"].as_array().expect("mentions");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0]["user_local_id"].as_i64(), Some(2));
    assert!(mentions[0]["team_local_id"].is_null());
    assert!(
        messages[1]["mentions"]
            .as_array()
            .is_some_and(|m| m.is_empty()),
        "second message has no mentions"
    );

    // ── 6. Unknown thread 404s with the reference message ──────────────────
    let r = client
        .get(format!("{base}/api/side-threads/999999"))
        .send()
        .await
        .expect("unknown thread probe");
    assert_eq!(r.status().as_u16(), 404);
    let body: Value = r.json().await.expect("404 body");
    assert_eq!(body["statusCode"].as_i64(), Some(404));
    assert_eq!(body["error"].as_str(), Some("NotFound"));
    assert_eq!(
        body["message"].as_str(),
        Some("Side thread not found."),
        "reference 404 message"
    );

    // ── 7. Thread id 0 is a 422 ────────────────────────────────────────────
    let r = client
        .get(format!("{base}/api/side-threads/0"))
        .send()
        .await
        .expect("zero thread id probe");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("422 body");
    assert_eq!(body["statusCode"].as_i64(), Some(422));
    assert_eq!(
        body["message"].as_str(),
        Some("id must be a positive integer.")
    );

    // ── 8. Soft-deleted conversations 404 (reference deleted_at filter) ────
    http_conn
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .execute(
            "UPDATE conversations SET deleted_at = '2026-01-01 00:00:00' WHERE id = 9",
            [],
        )
        .expect("soft-delete fixture conversation");
    let r = client
        .get(format!("{base}/api/conversations/9/side-threads"))
        .send()
        .await
        .expect("soft-deleted conversation probe");
    assert_eq!(
        r.status().as_u16(),
        404,
        "soft-deleted conversations must not list side threads (reference deleted_at filter)"
    );
}

/// CL-01: POST create with the full schema (title/team/participants/
/// first_message, 422 on unknown participants/teams, 404 on unknown
/// conversations) — live HTTP against the real server (same boot pattern as
/// the test above).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn side_thread_create_matches_the_reference_contract() {
    const PORT: u16 = 3992;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // Fixtures: two users, a team, one conversation.
    conn.execute_batch(
        "INSERT INTO users (id, remote_id, first_name, last_name, mention, email) VALUES
             (1, 501, 'Ada',  'Lovelace', 'ada',   'ada@example.com'),
             (2, 502, 'Grace', 'Hopper',  'grace', 'grace@example.com');
         INSERT INTO teams (id, remote_id, name) VALUES (7, 71, 'Support');
         INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id, status)
             VALUES (9, 100001, 4242, 'Refund question', 1, 1, 'active');",
    )
    .expect("seed fixtures");

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
    let server = spp_core::http::server::HttpServer::new(state);
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
    let up = Instant::now();
    while up.elapsed() < Duration::from_secs(15) {
        if client
            .get(format!("{base}/api/conversations/9/side-threads"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // ── 1. Create with the FULL schema (reference camelCase names) ─────────
    let r = client
        .post(format!("{base}/api/conversations/9/side-threads"))
        .json(&serde_json::json!({
            "title": "Pricing escalation",
            "teamId": 7,
            "participantUserIds": [1, 2],
            "createdByUserId": 1,
            "firstMessage": "Kicking this off"
        }))
        .send()
        .await
        .expect("create side thread");
    assert_eq!(r.status().as_u16(), 200, "full-schema create must succeed");
    let body: Value = r.json().await.expect("create body");
    assert_eq!(body["ok"], serde_json::json!(true));
    let thread_id = body["id"].as_i64().expect("thread id");
    assert!(thread_id > 0);
    // The response carries the reference-shaped detail.
    assert_eq!(
        body["side_thread"]["title"].as_str(),
        Some("Pricing escalation")
    );
    assert_eq!(body["side_thread"]["team_name"].as_str(), Some("Support"));

    // Everything is stored: title/team/creator, participants, first message.
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let (title, team, creator): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT title, team_local_id, created_by_user_id FROM side_threads WHERE id = ?1",
                [thread_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("thread row");
        assert_eq!(title, "Pricing escalation");
        assert_eq!(team, Some(7));
        assert_eq!(creator, Some(1));
        let participants: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM side_thread_participants WHERE side_thread_id = ?1",
                [thread_id],
                |r| r.get(0),
            )
            .expect("participants count");
        assert_eq!(participants, 2);
        let (msg_body, msg_author): (String, Option<i64>) = conn
            .query_row(
                "SELECT body, author_user_id FROM side_thread_messages WHERE thread_id = ?1",
                [thread_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("first message row");
        assert_eq!(msg_body, "Kicking this off");
        assert_eq!(msg_author, Some(1));
    }

    // The detail route serves the same state over HTTP.
    let r = client
        .get(format!("{base}/api/side-threads/{thread_id}"))
        .send()
        .await
        .expect("detail probe");
    assert_eq!(r.status().as_u16(), 200);
    let detail: Value = r.json().await.expect("detail body");
    assert_eq!(
        detail["side_thread"]["title"].as_str(),
        Some("Pricing escalation")
    );
    assert_eq!(
        detail["side_thread"]["participants"]
            .as_array()
            .expect("participants")
            .len(),
        2
    );
    assert_eq!(
        detail["side_thread"]["messages"]
            .as_array()
            .expect("messages")
            .len(),
        1
    );

    // ── 2. The port UI's snake_case names work identically ─────────────────
    let r = client
        .post(format!("{base}/api/conversations/9/side-threads"))
        .json(&serde_json::json!({
            "title": "Quick huddle",
            "team_local_id": null,
            "participant_user_ids": [2],
            "first_message": null
        }))
        .send()
        .await
        .expect("snake-case create");
    assert_eq!(r.status().as_u16(), 200, "snake_case create must work too");

    // ── 3. Validation: 422 with path: message issues ───────────────────────
    // Missing title.
    let r = client
        .post(format!("{base}/api/conversations/9/side-threads"))
        .json(&serde_json::json!({"teamId": 7}))
        .send()
        .await
        .expect("missing title");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("422 body");
    assert_eq!(body["error"], serde_json::json!("ValidationError"));
    assert!(
        body["message"]
            .as_str()
            .expect("message")
            .contains("title: Required"),
        "{}",
        body
    );
    // Unknown team + unknown participants + unknown creator in one pass.
    let r = client
        .post(format!("{base}/api/conversations/9/side-threads"))
        .json(&serde_json::json!({
            "title": "Bad refs",
            "teamId": 999,
            "participantUserIds": [77, 88],
            "createdByUserId": 66
        }))
        .send()
        .await
        .expect("unknown references");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("422 body");
    let msg = body["message"].as_str().expect("message");
    assert!(msg.contains("teamId: Unknown team 999"), "{msg}");
    assert!(
        msg.contains("participantUserIds: Unknown user(s): 77, 88"),
        "{msg}"
    );
    assert!(msg.contains("createdByUserId: Unknown user 66"), "{msg}");
    // Nothing was written for the rejected create.
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM side_threads WHERE title = 'Bad refs'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 0, "a rejected create must not write");
    }

    // ── 4. Unknown conversation 404; invalid id 422 ────────────────────────
    let r = client
        .post(format!("{base}/api/conversations/424242/side-threads"))
        .json(&serde_json::json!({"title": "X"}))
        .send()
        .await
        .expect("unknown conversation");
    assert_eq!(r.status().as_u16(), 404);
    let r = client
        .post(format!("{base}/api/conversations/0/side-threads"))
        .json(&serde_json::json!({"title": "X"}))
        .send()
        .await
        .expect("invalid conversation id");
    assert_eq!(r.status().as_u16(), 422);
}

/// CL-04: POST /api/side-threads/:id/participants — insert
/// side_thread_participants rows with existence checks
/// (sideThreadRepo.ts:173-186) — live HTTP against the real server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn side_thread_participants_add_matches_the_reference_contract() {
    const PORT: u16 = 3993;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // Fixtures: two users + a conversation + one side thread with user 2 as
    // its only initial participant.
    conn.execute_batch(
        "INSERT INTO users (id, remote_id, first_name, last_name, mention, email) VALUES
             (1, 501, 'Ada',  'Lovelace', 'ada',   'ada@example.com'),
             (2, 502, 'Grace', 'Hopper',  'grace', 'grace@example.com');
         INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id, status)
             VALUES (9, 100001, 4242, 'Refund question', 1, 1, 'active');
         INSERT INTO side_threads (id, conversation_id, title, created_by_user_id)
             VALUES (30, 9, 'Refund question', 1);
         INSERT INTO side_thread_participants (side_thread_id, user_local_id, added_by_user_local_id)
             VALUES (30, 2, 1);",
    )
    .expect("seed fixtures");

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
    let up = Instant::now();
    while up.elapsed() < Duration::from_secs(15) {
        if client
            .get(format!("{base}/api/side-threads/30"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // ── 1. Adding a new user inserts the row with the auditor ──────────────
    let r = client
        .post(format!("{base}/api/side-threads/30/participants"))
        .json(&serde_json::json!({"userIds": [1], "addedByUserId": 2}))
        .send()
        .await
        .expect("add participant");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("add body");
    assert_eq!(body["ok"], serde_json::json!(true));
    assert_eq!(body["added"].as_array().expect("added").len(), 1);
    assert_eq!(body["added"][0], serde_json::json!(1));
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let (added_by,): (Option<i64>,) = conn
            .query_row(
                "SELECT added_by_user_local_id FROM side_thread_participants
                 WHERE side_thread_id = 30 AND user_local_id = 1",
                [],
                |r| Ok((r.get(0)?,)),
            )
            .expect("added_by row");
        assert_eq!(added_by, Some(2));
    }

    // ── 2. The detail route now serves both participants ───────────────────
    let r = client
        .get(format!("{base}/api/side-threads/30"))
        .send()
        .await
        .expect("detail probe");
    assert_eq!(r.status().as_u16(), 200);
    let detail: Value = r.json().await.expect("detail body");
    assert_eq!(
        detail["side_thread"]["participants"]
            .as_array()
            .expect("participants")
            .len(),
        2
    );

    // ── 3. Duplicate add is idempotent (no second row, ok: true) ──────────
    let r = client
        .post(format!("{base}/api/side-threads/30/participants"))
        .json(&serde_json::json!({"userIds": [1]}))
        .send()
        .await
        .expect("duplicate add");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("dup body");
    assert_eq!(body["ok"], serde_json::json!(true));
    assert_eq!(
        body["added"].as_array().expect("added"),
        &Vec::<serde_json::Value>::new(),
        "nothing newly added"
    );
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM side_thread_participants WHERE side_thread_id = 30",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 2);
    }

    // ── 4. Unknown user -> 422, nothing written ────────────────────────────
    let r = client
        .post(format!("{base}/api/side-threads/30/participants"))
        .json(&serde_json::json!({"userIds": [55]}))
        .send()
        .await
        .expect("unknown participant");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("422 body");
    assert!(
        body["message"]
            .as_str()
            .expect("message")
            .contains("Unknown user"),
        "{}",
        body
    );

    // ── 5. Unknown thread 404; invalid id 422; empty/missing list 422 ──────
    let r = client
        .post(format!("{base}/api/side-threads/999999/participants"))
        .json(&serde_json::json!({"userIds": [1]}))
        .send()
        .await
        .expect("unknown thread");
    assert_eq!(r.status().as_u16(), 404);
    let r = client
        .post(format!("{base}/api/side-threads/0/participants"))
        .json(&serde_json::json!({"userIds": [1]}))
        .send()
        .await
        .expect("invalid thread id");
    assert_eq!(r.status().as_u16(), 422);
    let r = client
        .post(format!("{base}/api/side-threads/30/participants"))
        .json(&serde_json::json!({"userIds": []}))
        .send()
        .await
        .expect("empty list");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("422 body");
    assert!(
        body["message"]
            .as_str()
            .expect("message")
            .contains("userIds: Required"),
        "{}",
        body
    );
}
