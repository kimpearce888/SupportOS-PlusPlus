//! Audit item C2 (T16): swallowed transaction errors in conversation routes
//! reported fake success.
//!
//! The audit found that the conversation mutation routes wrapped their
//! local writes in `if let Ok(tx) = conn.transaction() { let _ = tx.execute(..);
//! let _ = tx.commit(); }` — a failed write (or a failed BEGIN) fell through
//! SILENTLY and the route still answered `ok:true` AND emitted the
//! `conversation` SSE event. The client (and the mirror) believed a mutation
//! that never happened.
//!
//! This test boots the REAL HTTP server against the Fake provider and
//! proves, for every fixed site:
//!
//!   - `POST /api/conversations/:id/priority`  (conversations.rs)
//!   - `POST /api/conversations/:id/state`      (conversations.rs)
//!   - `POST /api/conversations/:id/status`     (conversation_ops.rs)
//!
//! that (1) happy paths still persist + notify, and (2) a DB error at ANY
//! statement of the mutation transaction surfaces as the 500
//! `{statusCode,error,message}` envelope — never `ok:true` — leaves NO
//! partial writes behind (rollback), and fires NO `conversation` SSE event.
//!
//! Deterministic DB failures are injected with SQLite `RAISE(ABORT)`
//! triggers on the exact table each statement touches, so both the
//! first-statement and the later-statement (rollback) paths are exercised.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::broadcast;

use spp_core::events::ServerEvent;

const PORT: u16 = 3986;

// Multi-thread runtime: the server's async task must keep running while the
// test thread drives the HTTP probes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_mutations_never_report_fake_success_on_db_errors() {
    spp_core::logging::init();

    // ── Boot the real server against a throwaway data dir ─────────────────
    // The conversation row mirrors the Fake provider's remote id 105000 (the
    // demo world's first conversation) so the /status route's provider call
    // lands on a real in-memory row.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("c2-tx.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");
    conn.execute_batch(
        "INSERT OR IGNORE INTO mailboxes (id, remote_id, name) VALUES (1, 101, 'Support');
         INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (1, 201, 'Ada');
         INSERT INTO conversations
            (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id)
         VALUES (1, 105000, 1, 'C2 transaction error test', 'active', 1, 1)",
    )
    .expect("seed conversation");

    let http_conn = Arc::new(Mutex::new(conn));
    let bus = spp_core::http::EventBus::default();
    // The fake-success chain ends in an SSE emit — subscribe BEFORE any
    // mutation so every `conversation` event is observable.
    let mut events = bus.subscribe();

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
        port: PORT,
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

    // ── Shared probes ──────────────────────────────────────────────────────
    let drain_conv_events = |rx: &mut broadcast::Receiver<ServerEvent>| -> usize {
        let mut n = 0usize;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, ServerEvent::ConversationUpdated(_)) {
                n += 1;
            }
        }
        n
    };
    // The SSE emit happens synchronously before the response for these
    // routes; a short grace window makes the "no event" assertions robust.
    let settle = || async {
        tokio::time::sleep(Duration::from_millis(150)).await;
    };

    let text_col = |sql: &str| -> Option<String> {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row(sql, [], |r| r.get(0)).ok()
    };
    let int_col = |sql: &str| -> Option<i64> {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row(sql, [], |r| r.get(0)).ok()
    };
    let count = |sql: &str| -> i64 {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
    };
    let exec = |sql: &str| {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.execute(sql, []).expect("fixture exec");
    };

    let post = |path: &str, body: Value| {
        let client = client.clone();
        let url = format!("{base}{path}");
        async move {
            let r = client.post(&url).json(&body).send().await.expect("request");
            (r.status().as_u16(), r.json::<Value>().await.expect("json"))
        }
    };

    // ══════════════════════════════════════════════════════════════════════
    // Phase A — happy paths still persist + notify (guard the fix against
    // over-correction: errors must propagate, successes must still succeed).
    // ══════════════════════════════════════════════════════════════════════

    let (code, body) = post(
        "/api/conversations/1/priority",
        json!({ "priority": "high" }),
    )
    .await;
    assert_eq!(code, 200, "priority happy path answers 200: {body}");
    assert_eq!(body["ok"].as_bool(), Some(true), "body: {body}");
    assert_eq!(body["message"], "Priority set to high.");
    assert_eq!(body["data"]["priority"], "high");
    assert_eq!(body["data"]["hs_field_synced"], false);
    assert_eq!(
        text_col("SELECT supportos_priority FROM conversations WHERE id = 1").as_deref(),
        Some("high"),
        "priority persisted"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM activity_events
              WHERE conversation_id = 1 AND event_type = 'priority_changed'"
        ),
        1,
        "priority_changed activity row persisted"
    );
    assert_eq!(drain_conv_events(&mut events), 1, "SSE conversation event");

    let (code, body) = post(
        "/api/conversations/1/state",
        json!({ "stateId": 2, "reason": "needs investigation" }),
    )
    .await;
    assert_eq!(code, 200, "state happy path answers 200: {body}");
    assert_eq!(body["ok"].as_bool(), Some(true), "body: {body}");
    assert_eq!(body["message"], "State set to Investigating.");
    assert_eq!(
        int_col("SELECT supportos_state_id FROM conversations WHERE id = 1"),
        Some(2),
        "supportos_state_id persisted"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM state_transitions WHERE conversation_id = 1"),
        1,
        "state transition persisted"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM activity_events
              WHERE conversation_id = 1 AND event_type = 'ticket_state_changed'"
        ),
        1,
        "ticket_state_changed activity row persisted"
    );
    assert_eq!(drain_conv_events(&mut events), 1, "SSE conversation event");

    let (code, body) = post(
        "/api/conversations/1/status",
        json!({ "status": "pending" }),
    )
    .await;
    assert_eq!(code, 200, "status happy path answers 200: {body}");
    assert_eq!(body["ok"].as_bool(), Some(true), "body: {body}");
    assert_eq!(body["message"], "Status changed to pending.");
    assert_eq!(
        text_col("SELECT status FROM conversations WHERE id = 1").as_deref(),
        Some("pending"),
        "status persisted"
    );
    assert_eq!(drain_conv_events(&mut events), 1, "SSE conversation event");

    // ══════════════════════════════════════════════════════════════════════
    // Phase B — /priority: DB error on the FIRST statement (the priority
    // UPDATE aborts). Old code swallowed it and answered ok:true + SSE.
    // ══════════════════════════════════════════════════════════════════════

    exec(
        "CREATE TRIGGER c2_priority_fail BEFORE UPDATE OF supportos_priority
            ON conversations
          BEGIN SELECT RAISE(ABORT, 'c2 forced priority failure'); END;",
    );
    let (code, body) = post(
        "/api/conversations/1/priority",
        json!({ "priority": "urgent" }),
    )
    .await;
    assert_eq!(
        code, 500,
        "failed priority write must 500, got {code}: {body}"
    );
    assert_ne!(body["ok"].as_bool(), Some(true), "no fake success: {body}");
    assert_eq!(body["statusCode"], 500, "envelope: {body}");
    assert_eq!(body["error"], "InternalError", "envelope: {body}");
    assert!(
        body["message"].as_str().unwrap_or("").contains("c2 forced"),
        "DB error propagated in message: {body}"
    );
    assert_eq!(
        text_col("SELECT supportos_priority FROM conversations WHERE id = 1").as_deref(),
        Some("high"),
        "priority UNCHANGED after failed write"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM activity_events
              WHERE conversation_id = 1 AND event_type = 'priority_changed'"
        ),
        1,
        "no extra activity row for the failed attempt"
    );
    settle().await;
    assert_eq!(drain_conv_events(&mut events), 0, "NO SSE event on failure");
    exec("DROP TRIGGER c2_priority_fail;");

    // Phase B2 — /priority: DB error on the SECOND statement (the UPDATE
    // succeeds, the activity INSERT aborts) — proves rollback of the pair.
    exec(
        "CREATE TRIGGER c2_priority_activity_fail
            BEFORE INSERT ON activity_events
            WHEN NEW.event_type = 'priority_changed'
          BEGIN SELECT RAISE(ABORT, 'c2 forced activity failure'); END;",
    );
    let (code, body) = post(
        "/api/conversations/1/priority",
        json!({ "priority": "low" }),
    )
    .await;
    assert_eq!(
        code, 500,
        "failed activity write must 500, got {code}: {body}"
    );
    assert_ne!(body["ok"].as_bool(), Some(true), "no fake success: {body}");
    assert_eq!(
        text_col("SELECT supportos_priority FROM conversations WHERE id = 1").as_deref(),
        Some("high"),
        "priority write ROLLED BACK (atomic pair)"
    );
    settle().await;
    assert_eq!(drain_conv_events(&mut events), 0, "NO SSE event on failure");
    exec("DROP TRIGGER c2_priority_activity_fail;");

    // Route is healthy again once the fault is gone (not permanently wedged).
    let (code, body) = post(
        "/api/conversations/1/priority",
        json!({ "priority": "low" }),
    )
    .await;
    assert_eq!(code, 200, "priority recovers after fault cleared: {body}");
    assert_eq!(
        text_col("SELECT supportos_priority FROM conversations WHERE id = 1").as_deref(),
        Some("low")
    );
    let _ = drain_conv_events(&mut events);

    // ══════════════════════════════════════════════════════════════════════
    // Phase C — /state: DB error on a LATER statement (the transition INSERT
    // succeeds, the conversations UPDATE aborts) — the strongest atomicity
    // check: the already-inserted transition row must be rolled back.
    // ══════════════════════════════════════════════════════════════════════

    exec(
        "CREATE TRIGGER c2_state_fail BEFORE UPDATE OF supportos_state_id
            ON conversations
          BEGIN SELECT RAISE(ABORT, 'c2 forced state failure'); END;",
    );
    let (code, body) = post("/api/conversations/1/state", json!({ "stateId": 3 })).await;
    assert_eq!(code, 500, "failed state write must 500, got {code}: {body}");
    assert_ne!(body["ok"].as_bool(), Some(true), "no fake success: {body}");
    assert_eq!(body["statusCode"], 500, "envelope: {body}");
    assert_eq!(body["error"], "InternalError", "envelope: {body}");
    assert!(
        body["message"].as_str().unwrap_or("").contains("c2 forced"),
        "DB error propagated in message: {body}"
    );
    assert_eq!(
        int_col("SELECT supportos_state_id FROM conversations WHERE id = 1"),
        Some(2),
        "supportos_state_id UNCHANGED"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM state_transitions WHERE conversation_id = 1"),
        1,
        "inserted transition row ROLLED BACK (atomic triple)"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM activity_events
              WHERE conversation_id = 1 AND event_type = 'ticket_state_changed'"
        ),
        1,
        "no activity row for the failed attempt"
    );
    settle().await;
    assert_eq!(drain_conv_events(&mut events), 0, "NO SSE event on failure");
    exec("DROP TRIGGER c2_state_fail;");

    // Phase C2 — /state: DB error on the FIRST statement (transition INSERT
    // aborts) — nothing lands at all.
    exec(
        "CREATE TRIGGER c2_transition_fail BEFORE INSERT ON state_transitions
          BEGIN SELECT RAISE(ABORT, 'c2 forced transition failure'); END;",
    );
    let (code, body) = post("/api/conversations/1/state", json!({ "stateId": 4 })).await;
    assert_eq!(
        code, 500,
        "failed transition write must 500, got {code}: {body}"
    );
    assert_ne!(body["ok"].as_bool(), Some(true), "no fake success: {body}");
    assert_eq!(
        count("SELECT COUNT(*) FROM state_transitions WHERE conversation_id = 1"),
        1,
        "no transition row for the failed attempt"
    );
    settle().await;
    assert_eq!(drain_conv_events(&mut events), 0, "NO SSE event on failure");
    exec("DROP TRIGGER c2_transition_fail;");

    // ══════════════════════════════════════════════════════════════════════
    // Phase D — /status (op_change_status): the provider call succeeds but
    // the LOCAL mirror write fails — the audit's "ok:true + SSE" fake
    // success for the status route.
    // ══════════════════════════════════════════════════════════════════════

    exec(
        "CREATE TRIGGER c2_status_fail BEFORE UPDATE OF status ON conversations
          BEGIN SELECT RAISE(ABORT, 'c2 forced status failure'); END;",
    );
    let (code, body) = post("/api/conversations/1/status", json!({ "status": "closed" })).await;
    assert_eq!(
        code, 500,
        "failed status write must 500, got {code}: {body}"
    );
    assert_ne!(body["ok"].as_bool(), Some(true), "no fake success: {body}");
    assert_eq!(body["statusCode"], 500, "envelope: {body}");
    assert_eq!(body["error"], "InternalError", "envelope: {body}");
    assert_eq!(
        text_col("SELECT status FROM conversations WHERE id = 1").as_deref(),
        Some("pending"),
        "local status UNCHANGED after failed mirror write"
    );
    settle().await;
    assert_eq!(drain_conv_events(&mut events), 0, "NO SSE event on failure");
    exec("DROP TRIGGER c2_status_fail;");

    // Phase D2 — /status to closed: the status UPDATE succeeds but the
    // closed_at stamp aborts — the status write must roll back with it.
    exec(
        "CREATE TRIGGER c2_closed_fail BEFORE UPDATE OF closed_at ON conversations
          BEGIN SELECT RAISE(ABORT, 'c2 forced closed_at failure'); END;",
    );
    let (code, body) = post("/api/conversations/1/status", json!({ "status": "closed" })).await;
    assert_eq!(
        code, 500,
        "failed closed_at write must 500, got {code}: {body}"
    );
    assert_ne!(body["ok"].as_bool(), Some(true), "no fake success: {body}");
    assert_eq!(
        text_col("SELECT status FROM conversations WHERE id = 1").as_deref(),
        Some("pending"),
        "status write ROLLED BACK with the failed closed_at stamp (atomic)"
    );
    assert!(
        text_col("SELECT closed_at FROM conversations WHERE id = 1").is_none(),
        "closed_at never landed"
    );
    settle().await;
    assert_eq!(drain_conv_events(&mut events), 0, "NO SSE event on failure");
    exec("DROP TRIGGER c2_closed_fail;");

    // Final health check: the route works again once the fault is cleared.
    let (code, body) = post("/api/conversations/1/status", json!({ "status": "closed" })).await;
    assert_eq!(code, 200, "status recovers after fault cleared: {body}");
    assert_eq!(
        text_col("SELECT status FROM conversations WHERE id = 1").as_deref(),
        Some("closed")
    );
    let _ = drain_conv_events(&mut events);
}
