//! Audit item AC-03: the rebuildAll admin action.
//!
//! `POST /api/conversations/activity/rebuild` used to be a pure stub — it
//! answered `ok:true "Activity rebuild queued."` without queueing or running
//! anything, so the button in the admin panel never repaired a stale
//! activity layer. This test boots the REAL HTTP server AND the REAL
//! worker manager (the same boot shape the Tauri shell uses in demo mode),
//! seeds a mirror with two conversations whose threads exist but whose
//! activity layer is empty and whose derived columns sit at the defaults,
//! then drives the full path:
//!
//!   POST /api/conversations/activity/rebuild
//!     → {ok:true, message:"Activity rebuild queued."}
//!     → a `rebuild_activity` job lands on the maintenance queue
//!     → the worker's 2s tick claims + executes it
//!     → activity events re-derived from the thread mirror with the LOCAL
//!       conversation id and the sync-path dedup keys
//!     → derived columns recomputed (response state, first reply, waiting)
//!     → GET /api/conversations/:id/events (AC-04) serves the rebuilt
//!       timeline with the per-type counts.
//!
//! A second POST + completed job writes NOTHING new (end-to-end
//! idempotence — the whole point of the dedup keys).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

const PORT: u16 = 3996;

// Multi-thread runtime: the server's async task and the worker timers must
// keep running while the test drives them from the test thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebuild_all_admin_action_runs_the_real_rebuild() {
    spp_core::logging::init();

    // ── Boot the real server + the real worker manager ─────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // ── Fixtures: a stale mirror ───────────────────────────────────────────
    // Conversation 1 (local) / remote 5001: customer wrote, agent replied,
    // agent left a note — the thread mirror has all of it, but NO activity
    // events exist and the derived columns sit at their defaults.
    // Conversation 2 (local) / remote 5002: customer wrote last, plus a
    // draft the rebuild must skip.
    // DB-03: M047 FKs (foreign_keys=ON) — parents first, then the rows that
    // key on them.
    conn.execute(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 101, 'Support')",
        [],
    )
    .expect("seed mailbox");
    conn.execute(
        "INSERT INTO customers (id, remote_id, first_name, last_name, email, created_at)
         VALUES (10, 3001, 'Ada', 'Lovelace', 'ada@example.com', '2026-01-01T08:00:00Z')",
        [],
    )
    .expect("seed customer");
    conn.execute(
        "INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at)
         VALUES
            (1, 5001, 101, 'Refund question',  'active', 1, 10, '2026-01-01T09:00:00Z'),
            (2, 5002, 102, 'Bug report',       'active', 1, 10, '2026-01-01T09:30:00Z')",
        [],
    )
    .expect("seed conversations");
    conn.execute(
        "INSERT INTO users (id, remote_id, first_name, last_name, email, created_at)
         VALUES (2, 2001, 'Grace', 'Hopper', 'grace@example.com', '2026-01-01T08:00:00Z')",
        [],
    )
    .expect("seed user");
    conn.execute(
        "INSERT INTO conversation_threads
            (conversation_id, remote_id, type, state, body_text, from_type,
             created_by_user_id, created_by_customer_id, created_at)
         VALUES
            (1, 7001, 'customer', 'published', 'I want a refund',   'customer', NULL, 10, '2026-01-01T10:00:00Z'),
            (1, 7002, 'reply',    'published', 'We are on it',      'user',      2, NULL, '2026-01-01T10:05:00Z'),
            (1, 7003, 'note',     'published', 'VIP customer',      'user',      2, NULL, '2026-01-01T10:06:00Z'),
            (1, 7004, 'reply',    'draft',     'unsent draft',      'user',      2, NULL, '2026-01-01T11:00:00Z'),
            (2, 7005, 'customer', 'published', 'It crashed',        'customer', NULL, 10, '2026-01-01T10:30:00Z'),
            (2, 7006, 'reply',    'scheduled', 'hold the answer',   'user',      2, NULL, '2026-01-01T12:00:00Z')",
        [],
    )
    .expect("seed threads");

    let http_conn = Arc::new(Mutex::new(conn));
    let bus = spp_core::http::EventBus::default();
    let provider = Arc::new(spp_core::helpscout::FakeHelpScoutProvider::new_demo())
        as Arc<dyn spp_core::helpscout::HelpScoutProvider>;
    let sync = Arc::new(
        spp_core::sync_engine::SyncEngine::new(http_conn.clone(), provider.clone())
            .with_bus(bus.clone()),
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
    // The REAL worker manager: the 2s job tick is what claims + runs the
    // rebuild_activity job (the auto-sync timer stays parked — the sync
    // state on a fresh DB is NEW).
    let _workers = spp_core::workers::start_workers(
        http_conn.clone(),
        None,
        provider,
        spp_core::http::EventBus::default(),
        data_dir,
        None,
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");
    let base = format!("http://127.0.0.1:{PORT}");

    // ── Wait for boot ──────────────────────────────────────────────────────
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

    // Pre-state: stale mirror — no events, defaults everywhere.
    {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let events: i64 = c
            .query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0, "fixture: no activity events yet");
        let state: String = c
            .query_row(
                "SELECT response_state FROM conversations WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "needs_first_response",
            "fixture: stale derived columns"
        );
    }

    // ── 1. POST the admin action ───────────────────────────────────────────
    let resp = client
        .post(format!("{base}/api/conversations/activity/rebuild"))
        .send()
        .await
        .expect("rebuild request");
    assert_eq!(resp.status().as_u16(), 200, "route answers 200");
    let body: Value = resp.json().await.expect("rebuild json");
    assert_eq!(body["ok"].as_bool(), Some(true), "ok:true");
    assert_eq!(
        body["message"].as_str(),
        Some("Activity rebuild queued."),
        "the reference wire message"
    );

    // The job really landed on the maintenance queue.
    let (queued_kind, queued_queue): (String, String) = {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row(
            "SELECT type, queue FROM jobs WHERE type = 'rebuild_activity' ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("a rebuild_activity job row exists")
    };
    assert_eq!(queued_kind, "rebuild_activity");
    assert_eq!(queued_queue, "maintenance");

    // ── 2. Wait for the worker's tick to claim + complete the job ──────────
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status: Option<String> = {
            let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
            c.query_row(
                "SELECT status FROM jobs WHERE type = 'rebuild_activity' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok()
        };
        if status.as_deref() == Some("completed") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the rebuild_activity job never completed (last status: {status:?})"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // ── 3. The rebuild really ran: events + derived columns ────────────────
    {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        // 4 published threads (3 on conversation 1 + 1 on conversation 2)
        // → 4 events (the draft + scheduled ones skipped).
        let events: i64 = c
            .query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 4, "every published thread derived an event");

        // Local-id join + sync-path dedup keys + source='rebuild'.
        let row: (i64, String, String, String) = c
            .query_row(
                "SELECT conversation_id, event_type, dedup_key, source
                 FROM activity_events WHERE dedup_key = 'thread:7002'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("the agent reply event");
        assert_eq!(
            row,
            (
                1,
                "human_agent_message".into(),
                "thread:7002".into(),
                "rebuild".into()
            )
        );

        // Conversation 1: the note (user) is the last actor event → agent_waiting.
        let (state, first_reply): (String, Option<String>) = c
            .query_row(
                "SELECT response_state, first_response_at FROM conversations WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "agent_waiting");
        assert_eq!(first_reply.as_deref(), Some("2026-01-01T10:05:00Z"));

        // Conversation 2: customer wrote last → customer_waiting, waiting stamp set.
        let (state, waiting_since): (String, Option<String>) = c
            .query_row(
                "SELECT response_state, customer_waiting_since FROM conversations WHERE id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "customer_waiting");
        assert_eq!(waiting_since.as_deref(), Some("2026-01-01T10:30:00Z"));

        // The draft/scheduled threads never became events.
        let skipped: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM activity_events WHERE dedup_key IN ('thread:7004','thread:7006')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(skipped, 0, "draft + scheduled threads are not history");
    }

    // ── 4. AC-04's timeline endpoint serves the rebuilt data ───────────────
    let resp = client
        .get(format!("{base}/api/conversations/1/events"))
        .send()
        .await
        .expect("events request");
    assert_eq!(resp.status().as_u16(), 200, "timeline answers 200");
    let timeline: Value = resp.json().await.expect("timeline json");
    let events = timeline["events"].as_array().expect("events array");
    assert_eq!(events.len(), 3, "three events for conversation 1");
    assert_eq!(events[0]["event_type"].as_str(), Some("customer_message"));
    assert_eq!(events[0]["actor_name"].as_str(), Some("Ada Lovelace"));
    assert_eq!(
        events[1]["event_type"].as_str(),
        Some("human_agent_message")
    );
    assert_eq!(events[1]["actor_name"].as_str(), Some("Grace Hopper"));
    assert_eq!(events[2]["event_type"].as_str(), Some("internal_note"));
    // The per-type counts for the UI chips.
    let counts = &timeline["counts"];
    assert_eq!(counts["customer_message"].as_i64(), Some(1));
    assert_eq!(counts["human_agent_message"].as_i64(), Some(1));
    assert_eq!(counts["internal_note"].as_i64(), Some(1));

    // ── 5. A second run is a no-op (end-to-end idempotence) ────────────────
    let baseline: i64 = {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
            .unwrap()
    };
    let resp = client
        .post(format!("{base}/api/conversations/activity/rebuild"))
        .send()
        .await
        .expect("second rebuild request");
    assert_eq!(resp.status().as_u16(), 200);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let done: bool = {
            let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
            c.query_row(
                "SELECT COUNT(*) = 1 FROM jobs
                  WHERE type = 'rebuild_activity' AND status = 'completed'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(false)
        };
        if done {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second rebuild_activity job never completed"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Give the update a beat, then nothing new may have landed.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after: i64 = {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(after, baseline, "the second rebuild wrote no new events");
}
