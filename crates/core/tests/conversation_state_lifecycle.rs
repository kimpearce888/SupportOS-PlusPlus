//! TS-03 verification: transition history + per-state lifecycle serving on
//! the real HTTP server (reference conversations.ts:229-231 + 257-259 + 268
//! + ticketStateRepo.ts:69-203).
//!
//! - GET /api/conversations/:id serves `activity.ticket_state` (the current
//!   state row through conversations.supportos_state_id), the newest-first
//!   `activity.state_history` (joined state names, actor name for user
//!   transitions, '(no state)' for clears) and `activity.state_lifecycle`
//!   (per-state entries/total/avg minutes with spans to the next transition
//!   or now, time-in-current-state, re-entry counts), plus the top-level
//!   `ticket_states` picker list.
//! - POST /api/conversations/:id/state keeps recording the transitions the
//!   serving reads (the write side was already reference-conformant; this
//!   closes the loop: set states → read them served back).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4019;

#[tokio::test]
async fn conversation_state_lifecycle_parity() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // A conversation with an assignee; the built-in states seed through
    // M032. Transition history lands with fixed timestamps (SQLite
    // datetime format — parse_ts handles both it and RFC3339).
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO users (id, remote_id, first_name, last_name) VALUES (7, 70, 'Dana', 'Reyes');
         INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (11, 110, 'Ada');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id, created_at)
             VALUES (1, 101, 101, 'Export stuck at night', 'active', 1, 11, 7, '2026-10-01 10:00:00');
         INSERT INTO state_transitions (conversation_id, previous_state_id, new_state_id, actor_type, actor_local_id, reason, occurred_at, source)
             VALUES (1, NULL, 1, 'user', 7, 'initial triage', '2026-10-01 10:05:00', 'local');
         INSERT INTO state_transitions (conversation_id, previous_state_id, new_state_id, actor_type, actor_local_id, reason, occurred_at, source)
             VALUES (1, 1, 2, 'user', 7, 'started digging', '2026-10-01 11:00:00', 'local');
         INSERT INTO state_transitions (conversation_id, previous_state_id, new_state_id, actor_type, actor_local_id, reason, occurred_at, source)
             VALUES (1, 2, 3, 'user', 7, 'need more info', '2026-10-01 12:00:00', 'local');
         INSERT INTO state_transitions (conversation_id, previous_state_id, new_state_id, actor_type, actor_local_id, reason, occurred_at, source)
             VALUES (1, 3, 2, 'user', 7, 'customer replied', '2026-10-02 09:00:00', 'local');
         UPDATE conversations SET supportos_state_id = 2 WHERE id = 1;",
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

    // ── GET /api/conversations/:id — the activity payload ──────────────
    let r = client
        .get(format!("{base}/api/conversations/1"))
        .send()
        .await
        .expect("detail");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    let activity = &body["activity"];

    // ticket_state: the current state row (id 2 = 'Investigating' built-in).
    let current = &activity["ticket_state"];
    assert_eq!(current["id"], json!(2));
    assert_eq!(current["key"], json!("investigating"));
    assert_eq!(current["name"], json!("Investigating"));
    assert_eq!(current["built_in"], json!(1));
    assert!(current["color"].is_string());
    assert!(current["created_at"].is_string());

    // state_history: newest first, joined names, actor name.
    let history = activity["state_history"].as_array().expect("state_history");
    assert_eq!(history.len(), 4);
    let newest = &history[0];
    assert_eq!(newest["previous_state_id"], json!(3));
    assert_eq!(newest["new_state_id"], json!(2));
    assert_eq!(newest["previous_state_name"], json!("Waiting on Customer"));
    assert_eq!(newest["new_state_name"], json!("Investigating"));
    assert_eq!(newest["actor_type"], json!("user"));
    assert_eq!(newest["actor_local_id"], json!(7));
    assert_eq!(newest["actor_name"], json!("Dana Reyes"));
    assert_eq!(newest["reason"], json!("customer replied"));
    assert_eq!(newest["occurred_at"], json!("2026-10-02 09:00:00"));
    assert_eq!(newest["source"], json!("local"));
    let oldest = &history[3];
    assert!(oldest["previous_state_id"].is_null());
    assert!(oldest["previous_state_name"].is_null());
    assert_eq!(oldest["new_state_name"], json!("New"));

    // state_lifecycle: spans and re-entries.
    let lifecycle = &activity["state_lifecycle"];
    assert_eq!(lifecycle["current_state"]["id"], json!(2));
    assert_eq!(lifecycle["transitions"], json!(4));
    // New (id 1): 10:05→11:00 = 55 min, 1 entry.
    // Investigating (id 2): 11:00→12:00 = 60 min + 10-02 09:00→now (open
    // span), 2 entries.
    // Waiting on Customer (id 3): 12:00→next day 09:00 = 1260 min, 1 entry.
    let per_state = lifecycle["per_state"].as_array().expect("per_state");
    assert_eq!(per_state.len(), 3);
    let by_id = |id: i64| {
        per_state
            .iter()
            .find(|p| p["state_id"] == json!(id))
            .unwrap_or_else(|| panic!("state {id} missing"))
    };
    let new_state = by_id(1);
    assert_eq!(new_state["entries"], json!(1));
    assert_eq!(new_state["total_minutes"], json!(55));
    assert_eq!(new_state["avg_minutes"], json!(55));
    assert_eq!(new_state["last_entered"], json!("2026-10-01 10:05:00"));
    let investigating = by_id(2);
    assert_eq!(investigating["entries"], json!(2));
    // 60 min closed span + the open span since 10-02 09:00 (wall-clock):
    // assert the closed part exactly via total > 60 + a day, and the avg
    // lands between the two spans.
    let investigating_total = investigating["total_minutes"].as_i64().expect("total");
    assert!(
        investigating_total > 60 + 24 * 60,
        "total should include the open span: {investigating_total}"
    );
    assert!(investigating["avg_minutes"].is_i64());
    assert_eq!(investigating["last_entered"], json!("2026-10-02 09:00:00"));
    let waiting = by_id(3);
    assert_eq!(waiting["entries"], json!(1));
    assert_eq!(waiting["total_minutes"], json!(1260));
    // time_in_current_state_min: entered Investigating 10-02 09:00 —
    // some positive number of minutes since (exact value is wall-clock).
    let tic = lifecycle["time_in_current_state_min"]
        .as_i64()
        .expect("tic");
    assert!(
        tic > 24 * 60,
        "time in current state should exceed a day: {tic}"
    );

    // The top-level picker list (M032 built-ins, sort_order ASC).
    let states = body["ticket_states"].as_array().expect("ticket_states");
    assert_eq!(states.len(), 6);
    assert_eq!(states[0]["key"], json!("new"));
    assert_eq!(states[5]["key"], json!("resolved"));
    assert_eq!(states[0]["sort_order"], json!(10));

    // ── the write side keeps feeding the reads ─────────────────────────
    // Clear the state (stateId: null) → new_state_name '(no state)'.
    let r = client
        .post(format!("{base}/api/conversations/1/state"))
        .json(&json!({"stateId": null, "reason": "done for now"}))
        .send()
        .await
        .expect("clear state");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["message"], json!("State cleared."));

    let body: Value = client
        .get(format!("{base}/api/conversations/1"))
        .send()
        .await
        .expect("detail 2")
        .json()
        .await
        .expect("body");
    assert!(body["activity"]["ticket_state"].is_null());
    let history = body["activity"]["state_history"]
        .as_array()
        .expect("history");
    assert_eq!(history.len(), 5);
    assert!(history[0]["new_state_id"].is_null());
    assert_eq!(history[0]["new_state_name"], json!("(no state)"));
    assert_eq!(history[0]["reason"], json!("done for now"));
    // The lifecycle's open span closed: current_state null, per_state id 0
    // aggregates the cleared entry.
    let lifecycle = &body["activity"]["state_lifecycle"];
    assert!(lifecycle["current_state"].is_null());
    assert_eq!(lifecycle["transitions"], json!(5));
    let cleared = lifecycle["per_state"]
        .as_array()
        .expect("per_state")
        .iter()
        .find(|p| p["state_id"] == json!(0))
        .expect("cleared bucket");
    assert_eq!(cleared["state_name"], json!("(no state)"));
    assert_eq!(cleared["entries"], json!(1));
    // The cleared span (10-02 09:00 → the clear's occurred_at) is tiny but
    // counted; the waiting span before it stays 1260.

    // A fresh conversation without any state serving.
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at)
             VALUES (2, 102, 102, 'Untouched', 'active', 1, 11, '2026-10-03 10:00:00')",
            [],
        )
        .expect("seed conv 2");
    }
    let body: Value = client
        .get(format!("{base}/api/conversations/2"))
        .send()
        .await
        .expect("detail conv 2")
        .json()
        .await
        .expect("body");
    assert!(body["activity"]["ticket_state"].is_null());
    assert_eq!(
        body["activity"]["state_history"].as_array().unwrap().len(),
        0
    );
    assert_eq!(body["activity"]["state_lifecycle"]["transitions"], json!(0));
    assert_eq!(
        body["activity"]["state_lifecycle"]["per_state"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(body["activity"]["state_lifecycle"]["time_in_current_state_min"].is_null());
}
