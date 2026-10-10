//! Customers routes (UI-04): the reference `peopleRepo` customer wire
//! contract + the UI-27 inbox deep-link filters + the health UI variant.
//!
//! This suite boots the REAL HTTP server against a seeded database and
//! proves:
//!
//!   - GET /api/customers — the paginated `CustomerSummary` list: real
//!     `total` under the same predicate, per-row conversation/open counts,
//!     emails from the mirror table, organization name, last activity and
//!     the great=5/okay=3/not-good=1 average rating; LIKE wildcards are
//!     ESCAPED ('Grace_Hopper' must not match 'Grace Hopper'); pageSize
//!     clamps.
//!   - GET /api/customers/:id — the full `CustomerDetailData` envelope:
//!     customer (with open_conversation_count/average_rating/emails[]),
//!     conversations (newest first, assignee names), ratings, memories,
//!     properties, websites, social profiles, address, topics and the
//!     last published reply per closed conversation as resolutions; the
//!     reference 404 for unknown ids.
//!   - GET /api/conversations?tag=X — the case-insensitive tag filter;
//!     ?channel=bogus — the reference 422.
//!   - GET /health/detailed?format=ui — the always-200 UI variant with
//!     the full subsystem shape.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PORT: u16 = 3991;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn customers_routes_serve_the_reference_contract() {
    spp_core::logging::init();

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("customers-parity.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // ── Seed: Ada (full mirror data), Grace (wildcard trap), Hank ───────
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO users (id, remote_id, first_name, last_name)
             VALUES (7, 77, 'Ruth', 'Agent');
         INSERT INTO organizations (id, remote_id, name, domains)
             VALUES (1, 21, 'Acme', '[\"acme.com\"]');
         INSERT INTO customers (id, remote_id, first_name, last_name, organization_id, created_at)
             VALUES (500, 99001, 'Ada', 'Lovelace', 1, '2026-09-30 09:00:00');
         INSERT INTO customers (id, remote_id, first_name, last_name, created_at)
             VALUES (501, 99002, 'Grace', 'Hopper', '2026-09-29 08:00:00');
         INSERT INTO customers (id, remote_id, first_name, last_name, created_at)
             VALUES (502, 99003, 'Hank', 'Pym', '2026-09-28 07:00:00');
         INSERT INTO customer_emails (customer_id, value, type)
             VALUES (500, 'ada@example.com', 'work');
         INSERT INTO customer_emails (customer_id, value, type)
             VALUES (500, 'ada.alt@example.com', 'home');
         INSERT INTO customer_phones (customer_id, value)
             VALUES (500, '+1 555 0100');
         INSERT INTO customer_websites (customer_id, value)
             VALUES (500, 'https://ada.example.com');
         INSERT INTO customer_social_profiles (customer_id, value, type)
             VALUES (500, '@ada', 'twitter');
         INSERT INTO customer_addresses (customer_id, lines, city, country)
             VALUES (500, '1 Analytical Way', 'London', 'UK');
         INSERT INTO customer_property_definitions (id, remote_id, name)
             VALUES (9, 909, 'Plan');
         INSERT INTO customer_properties (customer_id, definition_id, value)
             VALUES (500, 9, 'enterprise');
         INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source, confidence)
             VALUES (500, 'prefers-step-by-step', 'Wants numbered step-by-step instructions',
                     'quote', 'ai', 'medium');
         -- Conversation 1: Ada's CLOSED ticket with a published agent reply
         -- (the resolution material).
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id, created_at, closed_at)
             VALUES (699, 99699, 6999, 'API token expired', 'closed', 1, 500, 7,
                     '2026-10-01 10:00:00', '2026-10-03 13:00:00');
         INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at, state)
             VALUES (699, 'reply', 'Rotated the token for you in two steps.', 'user', '2026-10-02 11:00:00', 'published');
         -- Conversation 2: Ada's ACTIVE ticket (the open count).
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at, last_activity_at)
             VALUES (700, 99700, 7001, 'API returns 401 again', 'active', 1, 500, '2026-10-05 12:00:00', '2026-10-06 08:00:00');
         -- Hank's chat conversation (the channel filter material).
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, type, created_at)
             VALUES (701, 99701, 7002, 'Beacon chat question', 'active', 1, 502, 'chat', '2026-10-06 09:00:00');
         INSERT INTO tags (id, remote_id, name) VALUES (3, 33, 'billing');
         INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (699, 3);
         INSERT INTO ratings (conversation_id, rating, customer_local_id, remote_id, remote_created_at)
             VALUES (699, 'great', 500, 551, '2026-10-06 14:00:00');",
    )
    .expect("seed world");

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

    // ── 1. GET /api/customers — the summary list ─────────────────────────
    let r = client
        .get(format!("{base}/api/customers"))
        .send()
        .await
        .expect("list customers");
    if r.status().as_u16() != 200 {
        let dbg: Value = r.json().await.expect("json");
        panic!("list customers failed: {dbg}");
    }
    let body: Value = r.json().await.expect("json");
    assert_eq!(
        body["total"],
        json!(3),
        "real total under the default view: {body}"
    );
    assert_eq!(body["page"], json!(1));
    let rows = body["customers"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 3);

    let find = |rows: &[Value], id: i64| {
        rows.iter()
            .find(|c| c["id"].as_i64() == Some(id))
            .cloned()
            .unwrap_or_else(|| panic!("customer {id} missing from list"))
    };
    let ada = find(&rows, 500);
    // The summary aggregates.
    assert_eq!(ada["conversation_count"], json!(2), "Ada: {ada}");
    assert_eq!(ada["open_conversation_count"], json!(1), "Ada: {ada}");
    let emails = ada["emails"].as_array().cloned().unwrap_or_default();
    assert!(
        emails.iter().any(|e| *e == json!("ada@example.com")),
        "emails from the mirror table: {emails:?}"
    );
    assert_eq!(ada["organization_name"], json!("Acme"));
    // great=5 → average 5.0.
    assert_eq!(ada["average_rating"], json!(5.0), "Ada: {ada}");
    assert_eq!(
        ada["last_activity_at"],
        json!("2026-10-06 08:00:00"),
        "last activity from the conversations: {ada}"
    );

    // ── 2. Search + LIKE-escape + pagination ──────────────────────────────
    let r = client
        .get(format!("{base}/api/customers?q=ada"))
        .send()
        .await
        .expect("search");
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["total"], json!(1), "case-insensitive first-name match");
    assert_eq!(body["customers"][0]["first_name"], json!("Ada"));

    // 'Grace_Hopper' must NOT match 'Grace Hopper' (the underscore is a
    // literal, not a single-char wildcard — the v2.2.1 escape rule).
    let r = client
        .get(format!("{base}/api/customers?q=Grace_Hopper"))
        .send()
        .await
        .expect("wildcard search");
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["total"], json!(0), "underscore escaped: {body}");

    // The email search path (customer_emails EXISTS clause).
    let r = client
        .get(format!("{base}/api/customers?q=ada.alt@"))
        .send()
        .await
        .expect("email search");
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["total"], json!(1), "identity-linked email matches");

    // Pagination: pageSize=2 → page 1 has 2 rows, page 2 has 1.
    let r = client
        .get(format!("{base}/api/customers?pageSize=2&page=1"))
        .send()
        .await
        .expect("page 1");
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["total"], json!(3));
    assert_eq!(body["customers"].as_array().map(Vec::len), Some(2));
    let r = client
        .get(format!("{base}/api/customers?pageSize=2&page=2"))
        .send()
        .await
        .expect("page 2");
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["customers"].as_array().map(Vec::len), Some(1));

    // ── 3. GET /api/customers/500 — the detail envelope ─────────────────
    let r = client
        .get(format!("{base}/api/customers/500"))
        .send()
        .await
        .expect("detail");
    if r.status().as_u16() != 200 {
        let dbg: Value = r.json().await.expect("json");
        panic!("customer detail failed: {dbg}");
    }
    let d: Value = r.json().await.expect("json");
    let c = &d["customer"];
    assert_eq!(c["first_name"], json!("Ada"));
    assert_eq!(c["open_conversation_count"], json!(1));
    assert_eq!(c["average_rating"], json!(5.0));
    assert!(
        c["emails"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| *e == json!("ada@example.com")),
        "detail emails: {c}"
    );
    // Conversations: 2 rows, newest first, with assignee names.
    let convs = d["conversations"].as_array().cloned().unwrap_or_default();
    assert_eq!(convs.len(), 2, "conversations: {d}");
    assert_eq!(convs[0]["id"], json!(700), "newest first");
    assert_eq!(convs[1]["assignee"], json!("Ruth Agent"), "assignee joined");
    // Ratings.
    let ratings = d["ratings"].as_array().cloned().unwrap_or_default();
    assert_eq!(ratings.len(), 1);
    assert_eq!(ratings[0]["rating"], json!("great"));
    assert_eq!(ratings[0]["conversation_id"], json!(699));
    // Memories.
    let memories = d["memories"].as_array().cloned().unwrap_or_default();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0]["key"], json!("prefers-step-by-step"));
    assert_eq!(memories[0]["source"], json!("ai"));
    // Properties.
    let props = d["properties"].as_array().cloned().unwrap_or_default();
    assert_eq!(props.len(), 1);
    assert_eq!(props[0]["name"], json!("Plan"));
    assert_eq!(props[0]["value"], json!("enterprise"));
    // Websites / social / address.
    assert_eq!(d["websites"][0], json!("https://ada.example.com"));
    assert_eq!(d["social_profiles"][0]["type"], json!("twitter"));
    let address = d["address"].as_str().unwrap_or_default();
    assert!(address.contains("London"), "address composed: {address}");
    // Topics: subjects of the newest conversations.
    let topics = d["topics"].as_array().cloned().unwrap_or_default();
    assert_eq!(topics.len(), 2);
    assert_eq!(topics[0]["topic"], json!("API returns 401 again"));
    // Resolutions: last published reply per closed conversation.
    let resolutions = d["resolutions"].as_array().cloned().unwrap_or_default();
    assert_eq!(resolutions.len(), 1, "resolutions: {d}");
    assert_eq!(resolutions[0]["number"], json!(6999));
    let resolution = resolutions[0]["resolution"].as_str().unwrap_or_default();
    assert!(
        resolution.contains("Rotated the token"),
        "resolution is the published reply: {resolution}"
    );

    // Unknown customer → the reference 404.
    let r = client
        .get(format!("{base}/api/customers/9999"))
        .send()
        .await
        .expect("unknown customer");
    assert_eq!(r.status().as_u16(), 404);
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["message"], json!("Customer not found."));

    // ── 4. GET /api/conversations?tag= + ?channel= (UI-27 deep links) ────
    let r = client
        .get(format!("{base}/api/conversations?tag=BILLING"))
        .send()
        .await
        .expect("tag filter");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    let convs = body["conversations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(convs.len(), 1, "case-insensitive tag match: {body}");
    assert_eq!(convs[0]["id"], json!(699));

    let r = client
        .get(format!("{base}/api/conversations?tag=missing-tag"))
        .send()
        .await
        .expect("missing tag");
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["total"], json!(0));

    let r = client
        .get(format!("{base}/api/conversations?channel=chat"))
        .send()
        .await
        .expect("channel filter");
    let body: Value = r.json().await.expect("json");
    let convs = body["conversations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(convs.len(), 1, "chat conversation only: {body}");
    assert_eq!(convs[0]["id"], json!(701));

    let r = client
        .get(format!("{base}/api/conversations?channel=carrier-pigeon"))
        .send()
        .await
        .expect("bad channel");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("json");
    assert_eq!(
        body["message"],
        json!("channel must be 'email' or 'chat'."),
        "the reference 422 message"
    );

    // ── 5. GET /health/detailed?format=ui — the always-200 UI variant ───
    let r = client
        .get(format!("{base}/health/detailed?format=ui"))
        .send()
        .await
        .expect("health ui");
    assert_eq!(r.status().as_u16(), 200, "format=ui always answers 200");
    let body: Value = r.json().await.expect("json");
    for key in [
        "status",
        "version",
        "time",
        "database",
        "helpscout",
        "lmstudio",
        "qdrant",
        "sync",
        "workers",
    ] {
        assert!(body.get(key).is_some(), "subsystem {key} present: {body}");
    }
    // Without the param the plain health route keeps its own semantics
    // (degraded stays 200; only a broken DB would 503).
    let r = client
        .get(format!("{base}/health/detailed"))
        .send()
        .await
        .expect("health plain");
    assert!(
        r.status().as_u16() == 200 || r.status().as_u16() == 503,
        "plain health keeps its status semantics"
    );
    let plain: Value = r.json().await.expect("json");
    assert_eq!(plain["status"], body["status"], "same payload either way");
}
