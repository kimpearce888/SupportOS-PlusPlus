//! UI-17 verification: the Notification Center API contracts the page
//! consumes (routes/notifications.ts parity).
//!
//! - GET /api/notifications — the reference field names (`type`, `title`,
//!   `body`, `target_user_local_id`, `conversation_number`, `read_at`),
//!   the `type` and `unreadOnly` filters, and the `{notifications, total,
//!   unread}` envelope.
//! - POST /api/notifications/:id/read — `{read}` semantics + `{unread}`.
//! - POST /api/notifications/read-all — `{marked, unread}`.
//! - GET/PUT /api/notifications/prefs — the 15-type list, the 422s and
//!   the persisted round-trip.
//! - GET /api/notifications/mentions — the "mentions for me" queue.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4024;

#[tokio::test]
async fn notification_center_contracts() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // A user (the acting "me"), a customer, a conversation, and three
    // notifications: an unread SLA breach (critical), an unread customer
    // reply (info) and a read mention (info).
    conn.execute_batch(
        "INSERT INTO users (id, remote_id, first_name, last_name, email) VALUES (7, 70, 'Dana', 'Reyes', 'dana@example.com');
         INSERT INTO customers (id, remote_id, first_name, last_name) VALUES (11, 110, 'Ada', 'Lovelace');
         INSERT OR IGNORE INTO mailboxes (id, remote_id, name) VALUES (1, 101, 'Support');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id)
             VALUES (1, 101, 101, 'Export stuck', 'active', 1, 11, 7);
         INSERT INTO notifications (id, type, severity, title, body, target_user_id,
                                    conversation_id, conversation_number, created_at, read_at, dedup_key)
             VALUES (1, 'sla_breach', 'critical', 'SLA breached on #101', 'First response overdue',
                     7, 1, 101, '2026-10-01 10:00:00', NULL, 'seed-1');
         INSERT INTO notifications (id, type, severity, title, body, target_user_id,
                                    conversation_id, conversation_number, created_at, read_at, dedup_key)
             VALUES (2, 'customer_replied', 'info', 'Ada replied on #101', NULL,
                     7, 1, 101, '2026-10-01 11:00:00', NULL, 'seed-2');
         INSERT INTO notifications (id, type, severity, title, body, target_user_id,
                                    conversation_id, conversation_number, created_at, read_at, dedup_key)
             VALUES (3, 'mentioned', 'info', 'You were mentioned', 'In a side thread',
                     7, 1, 101, '2026-10-01 12:00:00', '2026-10-01 13:00:00', 'seed-3');",
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

    // ── GET /api/notifications — the reference field names ──────────────
    let body: Value = client
        .get(format!("{base}/api/notifications?limit=50"))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(3));
    assert_eq!(body["unread"], json!(2));
    let notifications = body["notifications"].as_array().expect("rows");
    assert_eq!(notifications.len(), 3);
    let first = notifications
        .iter()
        .find(|n| n["id"] == json!(1))
        .expect("row 1");
    // The reference field names the UI parses.
    assert_eq!(first["type"], json!("sla_breach"));
    assert_eq!(first["severity"], json!("critical"));
    assert_eq!(first["title"], json!("SLA breached on #101"));
    assert_eq!(first["body"], json!("First response overdue"));
    assert_eq!(first["target_user_local_id"], json!(7));
    assert_eq!(first["conversation_id"], json!(1));
    assert_eq!(first["conversation_number"], json!(101));
    assert_eq!(first["read_at"], json!(null));

    // The type filter narrows to the SLA row.
    let body: Value = client
        .get(format!("{base}/api/notifications?type=customer_replied"))
        .send()
        .await
        .expect("type filter")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["notifications"][0]["id"], json!(2));

    // The unreadOnly filter serves the two unread rows.
    let body: Value = client
        .get(format!("{base}/api/notifications?unreadOnly=true"))
        .send()
        .await
        .expect("unread filter")
        .json()
        .await
        .expect("body");
    assert_eq!(body["notifications"].as_array().unwrap().len(), 2);

    // ── POST /api/notifications/1/read — the {read} semantics ──────────
    let body: Value = client
        .post(format!("{base}/api/notifications/1/read"))
        .json(&json!({ "read": true }))
        .send()
        .await
        .expect("mark read")
        .json()
        .await
        .expect("body");
    assert_eq!(body["unread"], json!(1));
    // The row now carries read_at on the wire.
    let body: Value = client
        .get(format!("{base}/api/notifications?type=sla_breach"))
        .send()
        .await
        .expect("refetch")
        .json()
        .await
        .expect("body");
    assert!(body["notifications"][0]["read_at"].is_string());

    // A 404 for an invisible id.
    let r = client
        .post(format!("{base}/api/notifications/999/read"))
        .json(&json!({ "read": true }))
        .send()
        .await
        .expect("unknown id");
    assert_eq!(r.status().as_u16(), 404);

    // ── POST /api/notifications/read-all ───────────────────────────────
    let body: Value = client
        .post(format!("{base}/api/notifications/read-all"))
        .send()
        .await
        .expect("read all")
        .json()
        .await
        .expect("body");
    // Row 1 was already read above, so read-all marks the remaining one.
    assert_eq!(body["marked"], json!(1));
    assert_eq!(body["unread"], json!(0));
    let body: Value = client
        .get(format!("{base}/api/notifications"))
        .send()
        .await
        .expect("list after read-all")
        .json()
        .await
        .expect("body");
    assert_eq!(body["unread"], json!(0));

    // ── Prefs: the 15-type list, the 422s and the round-trip ───────────
    let body: Value = client
        .get(format!("{base}/api/notifications/prefs"))
        .send()
        .await
        .expect("list prefs")
        .json()
        .await
        .expect("body");
    let prefs = body["prefs"].as_array().expect("prefs");
    assert_eq!(prefs.len(), 15);
    assert!(prefs.iter().all(|p| p["default_enabled"].is_boolean()));

    let r = client
        .put(format!("{base}/api/notifications/prefs/not_a_type"))
        .json(&json!({ "enabled": false }))
        .send()
        .await
        .expect("bad type");
    assert_eq!(r.status().as_u16(), 422);
    let r = client
        .put(format!("{base}/api/notifications/prefs/sla_breach"))
        .json(&json!({ "enabled": "yes" }))
        .send()
        .await
        .expect("bad body");
    assert_eq!(r.status().as_u16(), 422);

    // The persisted round-trip (the UI toggles then re-syncs from the PUT
    // response).
    let body: Value = client
        .put(format!("{base}/api/notifications/prefs/campaign_reply"))
        .json(&json!({ "enabled": false }))
        .send()
        .await
        .expect("set pref")
        .json()
        .await
        .expect("body");
    let campaign = body["prefs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["type"] == json!("campaign_reply"))
        .expect("campaign pref");
    assert_eq!(campaign["enabled"], json!(false));
    let body: Value = client
        .get(format!("{base}/api/notifications/prefs"))
        .send()
        .await
        .expect("prefs after set")
        .json()
        .await
        .expect("body");
    let campaign = body["prefs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["type"] == json!("campaign_reply"))
        .expect("campaign pref");
    assert_eq!(campaign["enabled"], json!(false));
    assert_eq!(campaign["default_enabled"], json!(true));
    // Restore.
    let _ = client
        .put(format!("{base}/api/notifications/prefs/campaign_reply"))
        .json(&json!({ "enabled": true }))
        .send()
        .await;

    // ── GET /api/notifications/mentions — the queue ─────────────────────
    let body: Value = client
        .get(format!("{base}/api/notifications/mentions"))
        .send()
        .await
        .expect("mentions")
        .json()
        .await
        .expect("body");
    assert_eq!(body["me"], json!(7));
    assert!(body["notifications"].is_array());
    assert!(body["side_thread_mentions"].is_array());
}
