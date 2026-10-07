//! TL-02 verification: customer/organization timeline reads on the real
//! HTTP server (reference people.ts:86-135 + customerEventsRepo.ts:53-89 +
//! routes/helpers.ts clampListParam).
//!
//! - POST /api/timeline/rebuild — the full idempotent re-derivation
//!   answering `{ok, created, message}` with the `customer_events_rebuilt`
//!   audit row; a second rebuild derives 0 new events.
//! - GET /api/customers/:id/timeline — `{events, total, kind_counts}` over
//!   customer_events: the kind filter (truncated to 40 chars, empty =
//!   unfiltered), the pageSize/page clamps (1..200 / 1..100000, garbage
//!   falls back), newest-first ordering, the served event shape, and the
//!   reference 404 envelope on unknown customers.
//! - GET /api/organizations/:id/timeline — the union of member customers'
//!   events `{events, total}` with `customer_name` on each row, the same
//!   kind/page semantics, and the reference 404 envelope (deleted orgs
//!   included).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4018;

#[tokio::test]
async fn people_timeline_parity() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // A small mirror world: org 1 with members Ada (11) and Belle (12),
    // lone customer Carlos (13); Ada's conversation is closed with a
    // published customer message and a rating; Belle's is open. Org 2 is
    // soft-deleted (404 like getOrganizationDetail).
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO organizations (id, remote_id, name, domains)
             VALUES (1, 21, 'Acme', '[\"acme.com\"]');
         INSERT INTO organizations (id, remote_id, name, domains, deleted_at)
             VALUES (2, 22, 'Gone Co', '[]', '2026-09-01 00:00:00');
         INSERT INTO customers (id, remote_id, first_name, last_name, organization_id, created_at)
             VALUES (11, 110, 'Ada', 'Lovelace', 1, '2026-09-30 09:00:00');
         INSERT INTO customers (id, remote_id, first_name, last_name, organization_id, created_at)
             VALUES (12, 120, 'Belle', 'Node', 1, '2026-09-29 08:00:00');
         INSERT INTO customers (id, remote_id, first_name, last_name, created_at)
             VALUES (13, 130, 'Carlos', 'Solo', '2026-09-28 07:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at, closed_at)
             VALUES (1, 101, 101, 'Export stuck at night', 'closed', 1, 11, '2026-10-01 10:00:00', '2026-10-03 13:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at)
             VALUES (2, 102, 102, 'Belle question', 'active', 1, 12, '2026-10-02 12:00:00');
         INSERT INTO conversation_threads (conversation_id, thread_type, body, actor_type, created_at, state)
             VALUES (1, 'customer', 'The export stalls after midnight', 'customer', '2026-10-02 11:00:00', 'published');
         INSERT INTO ratings (conversation_id, rating, customer_local_id, remote_id, remote_created_at)
             VALUES (1, 'great', 11, 551, '2026-10-06 14:00:00');",
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

    // ── POST /api/timeline/rebuild ─────────────────────────────────────
    // Derived: 3 signups + conv1 started/closed + conv2 started + the
    // customer message + the rating = 8 events.
    let r = client
        .post(format!("{base}/api/timeline/rebuild"))
        .send()
        .await
        .expect("rebuild");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["created"], json!(8));
    assert_eq!(
        body["message"],
        json!("Timeline rebuilt; 8 new event(s) derived.")
    );

    // The audit row (action + after_state shape).
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let (action, after): (String, Value) = conn
            .query_row(
                "SELECT action, after_state FROM audit_log WHERE action = 'customer_events_rebuilt'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        serde_json::from_str::<Value>(&r.get::<_, String>(1)?)
                            .unwrap_or_default(),
                    ))
                },
            )
            .expect("audit row");
        assert_eq!(action, "customer_events_rebuilt");
        assert_eq!(after, json!({"created": 8}));
    }

    // Idempotent: a second rebuild derives nothing.
    let body: Value = client
        .post(format!("{base}/api/timeline/rebuild"))
        .send()
        .await
        .expect("rebuild 2")
        .json()
        .await
        .expect("body");
    assert_eq!(body["created"], json!(0));
    assert_eq!(
        body["message"],
        json!("Timeline rebuilt; 0 new event(s) derived.")
    );

    // ── GET /api/customers/:id/timeline — the full read ────────────────
    let r = client
        .get(format!("{base}/api/customers/11/timeline"))
        .send()
        .await
        .expect("customer timeline");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    let events = body["events"].as_array().expect("events");
    assert_eq!(body["total"], json!(5));
    assert_eq!(events.len(), 5);
    // The served event shape: the full CustomerEventRecord key set.
    let mut served: Vec<&str> = events[0]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    served.sort();
    assert_eq!(
        served,
        vec![
            "created_at",
            "customer_local_id",
            "detail",
            "event_kind",
            "id",
            "occurred_at",
            "source",
            "source_ref",
            "title",
        ]
    );
    // Newest-first: rating (10-06) > conv closed (10-03) > customer message
    // (10-02) > conv started (10-01) > signup (09-30).
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["event_kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "rating",
            "support_conversation",
            "customer_message",
            "support_conversation",
            "signup"
        ]
    );
    assert_eq!(
        events[0]["title"],
        json!("Customer rated the support great")
    );
    assert_eq!(
        events[0]["detail"],
        json!({"rating": "great", "comments": null, "conversation_id": 1})
    );
    assert_eq!(events[0]["source"], json!("hs_sync"));
    assert_eq!(events[0]["source_ref"], json!("ratings:551"));
    assert_eq!(
        events[1]["title"],
        json!("Support conversation #101 closed")
    );
    assert_eq!(
        events[2]["detail"]["excerpt"],
        json!("The export stalls after midnight")
    );

    // kind_counts: support_conversation leads (n DESC).
    let counts = body["kind_counts"].as_array().expect("kind_counts");
    assert_eq!(counts[0], json!({"kind": "support_conversation", "n": 2}));
    let mut pairs: Vec<(String, i64)> = counts
        .iter()
        .map(|c| {
            (
                c["kind"].as_str().unwrap().to_string(),
                c["n"].as_i64().unwrap(),
            )
        })
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("customer_message".into(), 1),
            ("rating".into(), 1),
            ("signup".into(), 1),
            ("support_conversation".into(), 2),
        ]
    );

    // The kind filter narrows both the page and the total.
    let body: Value = client
        .get(format!("{base}/api/customers/11/timeline?kind=rating"))
        .send()
        .await
        .expect("kind filter")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
    assert_eq!(body["events"][0]["event_kind"], json!("rating"));

    // The kind filter truncates to 40 chars before matching.
    let long_kind = format!("rating{}", "x".repeat(35));
    let body: Value = client
        .get(format!("{base}/api/customers/11/timeline?kind={long_kind}"))
        .send()
        .await
        .expect("kind truncate")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(0));
    assert_eq!(body["events"].as_array().unwrap().len(), 0);

    // An empty kind means unfiltered (slice-then-truthy, like the route).
    let body: Value = client
        .get(format!("{base}/api/customers/11/timeline?kind="))
        .send()
        .await
        .expect("empty kind")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(5));

    // Pagination: pageSize=2, page=2 → the 3rd and 4th newest of 5.
    let body: Value = client
        .get(format!(
            "{base}/api/customers/11/timeline?pageSize=2&page=2"
        ))
        .send()
        .await
        .expect("page 2")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(5));
    let events = body["events"].as_array().expect("events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["event_kind"], json!("customer_message"));
    assert_eq!(events[1]["event_kind"], json!("support_conversation"));

    // pageSize=0 clamps to 1 (one event per page).
    let body: Value = client
        .get(format!(
            "{base}/api/customers/11/timeline?pageSize=0&page=1"
        ))
        .send()
        .await
        .expect("pageSize 0")
        .json()
        .await
        .expect("body");
    assert_eq!(body["events"].as_array().unwrap().len(), 1);

    // pageSize beyond the max clamps to 200; page=0 clamps to 1.
    let body: Value = client
        .get(format!(
            "{base}/api/customers/11/timeline?pageSize=1000&page=0"
        ))
        .send()
        .await
        .expect("pageSize 1000")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(5));
    assert_eq!(body["events"].as_array().unwrap().len(), 5);

    // Garbage pageSize falls back to the default (100).
    let body: Value = client
        .get(format!("{base}/api/customers/11/timeline?pageSize=abc"))
        .send()
        .await
        .expect("garbage pageSize")
        .json()
        .await
        .expect("body");
    assert_eq!(body["events"].as_array().unwrap().len(), 5);

    // Fractional pageSize truncates toward zero.
    let body: Value = client
        .get(format!(
            "{base}/api/customers/11/timeline?pageSize=2.9&page=1"
        ))
        .send()
        .await
        .expect("fractional pageSize")
        .json()
        .await
        .expect("body");
    assert_eq!(body["events"].as_array().unwrap().len(), 2);

    // Unknown customer: the reference 404 envelope.
    let r = client
        .get(format!("{base}/api/customers/999/timeline"))
        .send()
        .await
        .expect("unknown customer");
    assert_eq!(r.status().as_u16(), 404);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body,
        json!({"statusCode": 404, "error": "NotFound", "message": "Customer not found."})
    );

    // ── GET /api/organizations/:id/timeline ───────────────────────────
    // The union of member customers' events (Ada 5 + Belle 2), each row
    // carrying customer_name; no kind_counts key on this endpoint.
    let r = client
        .get(format!("{base}/api/organizations/1/timeline"))
        .send()
        .await
        .expect("org timeline");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("body");
    let events = body["events"].as_array().expect("events");
    assert_eq!(body["total"], json!(7));
    assert_eq!(events.len(), 7);
    assert!(body.get("kind_counts").is_none());
    assert_eq!(events[0]["customer_name"], json!("Ada Lovelace"));
    // Newest first across both members: rating (10-06), Ada conv closed
    // (10-03), Belle conv started (10-02 12:00), Ada customer message
    // (10-02 11:00), Ada conv started (10-01), then the two signups.
    assert_eq!(events[0]["event_kind"], json!("rating"));
    assert_eq!(events[1]["event_kind"], json!("support_conversation"));
    assert_eq!(events[1]["customer_name"], json!("Ada Lovelace"));
    assert_eq!(events[2]["customer_name"], json!("Belle Node"));
    assert_eq!(
        events[2]["title"],
        json!("Support conversation #102 started")
    );
    assert_eq!(
        events[4]["title"],
        json!("Support conversation #101 started")
    );

    // The kind filter works on the union too.
    let body: Value = client
        .get(format!("{base}/api/organizations/1/timeline?kind=signup"))
        .send()
        .await
        .expect("org kind filter")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(2));
    assert_eq!(body["events"].as_array().unwrap().len(), 2);

    // Pagination on the union.
    let body: Value = client
        .get(format!(
            "{base}/api/organizations/1/timeline?pageSize=3&page=2"
        ))
        .send()
        .await
        .expect("org page 2")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(7));
    assert_eq!(body["events"].as_array().unwrap().len(), 3);

    // Unknown org: the reference 404 envelope.
    let r = client
        .get(format!("{base}/api/organizations/999/timeline"))
        .send()
        .await
        .expect("unknown org");
    assert_eq!(r.status().as_u16(), 404);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body,
        json!({"statusCode": 404, "error": "NotFound", "message": "Organization not found."})
    );

    // A soft-deleted org is gone (getOrganizationDetail semantics).
    let r = client
        .get(format!("{base}/api/organizations/2/timeline"))
        .send()
        .await
        .expect("deleted org");
    assert_eq!(r.status().as_u16(), 404);
}
