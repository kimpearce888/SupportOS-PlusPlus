//! AN-01 / AN-12 / AC-04 verification: the dashboard + report-builder +
//! conversation-events batch on the real HTTP server.
//!
//! - GET /api/analytics/dashboard — `analyticsService.dashboard(from, to,
//!   scope)`: all 20 fields computed from the mirror (status counts, reply
//!   count, avg response/resolution minutes, ratings split, by_mailbox /
//!   by_tag / by_agent / by_team / daily_new / by_channel / channel_metrics /
//!   mailbox_comparison), days clamping, mailboxIds + channel scopes with
//!   the reference 422 envelopes, and the daily_metrics snapshot cache.
//! - POST /api/reports/builder/run — MAIN metric SQL semantics: agent /
//!   customer replies count only PUBLISHED, NOT-DELETED threads in range;
//!   the channel dimension groups via `source_type`; the channel filter
//!   matches `source_type`; the team dimension resolves through team_members.
//! - GET /api/conversations/:id/events — the full event timeline: 404 for
//!   unknown conversations, limit clamping (1..=1000, fallback 200), actor
//!   NAMES resolved through users/customers/system_users, metadata parsed
//!   from the stored JSON, and per-type counts.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use spp_core::helpscout::HelpScoutProvider;

const SERVER_PORT: u16 = 4011;

/// Insert a conversation with explicit channel/assignee/timing columns.
#[allow(clippy::too_many_arguments)]
fn insert_conv(
    conn: &rusqlite::Connection,
    remote: i64,
    mailbox: i64,
    customer: i64,
    status: &str,
    channel: &str,
    assignee: Option<i64>,
    created_at_sql: &str,
    updated_at_sql: &str,
) -> i64 {
    let assignee_sql = assignee
        .map(|a| a.to_string())
        .unwrap_or_else(|| "NULL".to_string());
    conn.execute(
        &format!(
            "INSERT INTO conversations
                (remote_id, number, mailbox_id, customer_id, status, type, source_type,
                 assignee_id, created_at, updated_at)
             VALUES ({remote}, {remote}, {mailbox}, {customer}, '{status}', '{channel}',
                     '{channel}', {assignee_sql}, {created_at_sql}, {updated_at_sql})"
        ),
        [],
    )
    .unwrap();
    conn.query_row(
        "SELECT id FROM conversations WHERE remote_id = ?1",
        rusqlite::params![remote],
        |r| r.get(0),
    )
    .unwrap()
}

#[tokio::test]
async fn dashboard_events_builder_batch() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // Reference world: 2 mailboxes, 2 users (Ada on team Frontline, Grace
    // unteamed), 2 customers, and 7 conversations with explicit channels,
    // assignees and timing.
    conn.execute(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support'), (2, 202, 'Billing')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO teams (id, remote_id, name) VALUES (1, 301, 'Frontline')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO users (id, remote_id, first_name, last_name) VALUES
            (10, 401, 'Ada', 'Lovelace'), (11, 402, 'Grace', 'Hopper')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO team_members (team_id, user_id) VALUES (1, 10)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO customers (id, remote_id, first_name, last_name) VALUES
            (100, 501, 'Carol', 'Client'), (101, 502, 'Dave', 'Doe')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO system_users (id, remote_id, first_name, last_name) VALUES (7, 601, 'Sibyl', 'System')",
        [],
    )
    .unwrap();

    // In-range conversations (created 2 days ago unless noted).
    // c1: Support, active, email, Ada.
    let c1 = insert_conv(
        &conn,
        1,
        1,
        100,
        "active",
        "email",
        Some(10),
        "datetime('now', '-2 days')",
        "datetime('now')",
    );
    // c2: Support, pending, chat, Ada.
    let c2 = insert_conv(
        &conn,
        2,
        1,
        101,
        "pending",
        "chat",
        Some(10),
        "datetime('now', '-2 days')",
        "datetime('now')",
    );
    // c3: Billing, closed yesterday, email, Grace.
    let c3 = insert_conv(
        &conn,
        3,
        2,
        100,
        "closed",
        "email",
        Some(11),
        "datetime('now', '-2 days')",
        "datetime('now')",
    );
    conn.execute(
        "UPDATE conversations SET closed_at = datetime('now', '-1 day'),
             first_customer_message_at = datetime('now', '-2 days')
         WHERE id = ?1",
        rusqlite::params![c3],
    )
    .unwrap();
    // c4: Billing, active, unassigned, no activity for 10 days (backlog +
    // waiting since 10 days).
    let c4 = insert_conv(
        &conn,
        4,
        2,
        101,
        "active",
        "email",
        None,
        "datetime('now', '-2 days')",
        "datetime('now', '-10 days')",
    );
    conn.execute(
        "UPDATE conversations SET customer_waiting_since = datetime('now', '-10 days'),
             first_customer_message_at = datetime('now', '-10 days')
         WHERE id = ?1",
        rusqlite::params![c4],
    )
    .unwrap();
    // c5: Support, active, chat, created 40 days ago (OUTSIDE the default
    // 30-day range; recently updated so it is not backlog).
    let _c5 = insert_conv(
        &conn,
        5,
        1,
        100,
        "active",
        "chat",
        Some(11),
        "datetime('now', '-40 days')",
        "datetime('now')",
    );
    // c6: soft-deleted (excluded everywhere).
    let _c6 = insert_conv(
        &conn,
        6,
        1,
        100,
        "active",
        "email",
        Some(10),
        "datetime('now', '-2 days')",
        "datetime('now')",
    );
    conn.execute(
        "UPDATE conversations SET deleted_at = datetime('now') WHERE id = ?1",
        rusqlite::params![_c6],
    )
    .unwrap();
    // c7: Billing, active, unassigned email.
    let _c7 = insert_conv(
        &conn,
        7,
        2,
        101,
        "active",
        "email",
        None,
        "datetime('now', '-2 days')",
        "datetime('now')",
    );

    // Threads: replies drive replies_sent + first-response averages.
    // - c1 reply A: published, in range (first reply after 30 minutes).
    conn.execute(
        "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
         VALUES (?1, 'reply', 'published', 'Answering c1', 'user', datetime('now', '-2 days', '+30 minutes'))",
        rusqlite::params![c1],
    )
    .unwrap();
    conn.execute(
        "UPDATE conversations SET first_customer_message_at = datetime('now', '-2 days') WHERE id = ?1",
        rusqlite::params![c1],
    )
    .unwrap();
    // - c1 reply B: draft (NOT history).
    conn.execute(
        "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
         VALUES (?1, 'reply', 'draft', 'draft only', 'user', datetime('now', '-2 days'))",
        rusqlite::params![c1],
    )
    .unwrap();
    // - c2 reply C: deleted (excluded).
    conn.execute(
        "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at, deleted_at)
         VALUES (?1, 'reply', 'published', 'deleted reply', 'user', datetime('now', '-2 days'), datetime('now'))",
        rusqlite::params![c2],
    )
    .unwrap();
    // - c3 reply D: published, in range (first reply after 60 minutes).
    conn.execute(
        "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
         VALUES (?1, 'reply', 'published', 'Answering c3', 'user', datetime('now', '-2 days', '+60 minutes'))",
        rusqlite::params![c3],
    )
    .unwrap();
    // - c5 reply E: published but 40 days old (outside the range).
    conn.execute(
        "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
         VALUES (?1, 'reply', 'published', 'old reply', 'user', datetime('now', '-40 days'))",
        rusqlite::params![_c5],
    )
    .unwrap();
    // Customer messages (builder customer_replies metric): one published on
    // c1, one draft (excluded), one deleted (excluded), one published on c2.
    for (conv, state, deleted) in [
        (c1, "published", false),
        (c1, "draft", false),
        (c1, "published", true),
        (c2, "published", false),
    ] {
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at, deleted_at)
             VALUES (?1, 'customer_message', ?2, 'customer text', 'customer', datetime('now', '-2 days'), ?3)",
            rusqlite::params![conv, state, deleted.then(|| "datetime('now')".to_string())],
        )
        .unwrap();
    }

    // Tags: 'bug' on c1 + c2 (in range), 'old' on c5 (out of range).
    conn.execute(
        "INSERT INTO tags (id, remote_id, name) VALUES (1, 701, 'bug'), (2, 702, 'old')",
        [],
    )
    .unwrap();
    for (tag, conv) in [(1, c1), (1, c2), (2, _c5)] {
        conn.execute(
            "INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (?1, ?2)",
            rusqlite::params![conv, tag],
        )
        .unwrap();
    }

    // Ratings: great on c1, okay on c2, not-good on c3 (all in range).
    for (i, (conv, rating)) in [(0, (c1, "great")), (1, (c2, "okay")), (2, (c3, "not-good"))] {
        conn.execute(
            "INSERT INTO ratings (remote_id, conversation_id, rating, remote_created_at)
             VALUES (?1, ?2, ?3, datetime('now', '-2 days'))",
            rusqlite::params![900 + i, conv, rating],
        )
        .unwrap();
    }

    // The ratings-refresh timer's first tick fires at boot: disable it so
    // the fake provider's demo ratings never mix into this test's world
    // (ratings_refresh_seconds = 0 skips the timer entirely).
    spp_core::settings::set_i64(&conn, "ratings_refresh_seconds", 0)
        .expect("disable ratings refresh");

    let http_conn = Arc::new(Mutex::new(conn));
    let bus = spp_core::http::EventBus::default();
    let provider = Arc::new(spp_core::helpscout::FakeHelpScoutProvider::new_demo())
        as Arc<dyn HelpScoutProvider>;
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

    // ════════════════════════════════════════════════════════════════════
    // AN-01: GET /api/analytics/dashboard — default scope
    // ════════════════════════════════════════════════════════════════════
    let resp = client
        .get(format!("{base}/api/analytics/dashboard"))
        .send()
        .await
        .expect("dashboard");
    assert_eq!(resp.status().as_u16(), 200);
    let d: Value = resp.json().await.expect("dashboard body");

    // All 20 reference fields are present.
    for key in [
        "range",
        "new_conversations",
        "active_conversations",
        "pending_conversations",
        "closed_conversations",
        "unassigned",
        "backlog",
        "first_response_time_avg_min",
        "resolution_time_avg_min",
        "replies_sent",
        "ratings",
        "by_mailbox",
        "by_tag",
        "by_agent",
        "by_team",
        "daily_new",
        "by_channel",
        "channel_metrics",
        "mailbox_comparison",
        "source",
    ] {
        assert!(d.get(key).is_some(), "dashboard field missing: {key}");
    }
    assert!(d["range"]["from"].is_string() && d["range"]["to"].is_string());
    assert_eq!(d["source"], json!(["local"]));

    // Status counts over the seeded world (days=30 default).
    assert_eq!(
        d["new_conversations"],
        json!(5),
        "c1-c4 + c7 in range (c5 out, c6 deleted)"
    );
    assert_eq!(d["active_conversations"], json!(4), "c1, c4, c5, c7");
    assert_eq!(d["pending_conversations"], json!(1), "c2");
    assert_eq!(d["closed_conversations"], json!(1), "c3 closed yesterday");
    assert_eq!(d["unassigned"], json!(2), "c4 + c7 (active, no assignee)");
    assert_eq!(d["backlog"], json!(1), "c4: no activity for 10 days");

    // Reply + timing aggregates.
    assert_eq!(
        d["replies_sent"],
        json!(2),
        "reply A (c1) + reply D (c3) only"
    );
    assert_eq!(d["first_response_time_avg_min"], json!(45), "AVG(30, 60)");
    assert_eq!(
        d["resolution_time_avg_min"],
        json!(1440),
        "c3: created -2d, closed -1d"
    );

    // Ratings split.
    assert_eq!(d["ratings"], json!({"great": 1, "okay": 1, "not-good": 1}));

    // Grouped breakdowns.
    let by_mailbox = d["by_mailbox"].as_array().expect("by_mailbox");
    assert_eq!(
        by_mailbox[0],
        json!({"name": "Billing", "count": 3}),
        "count DESC"
    );
    assert_eq!(
        by_mailbox.iter().find(|r| r["name"] == json!("Billing")),
        Some(&json!({"name": "Billing", "count": 3})),
        "by_mailbox shows every mailbox: {by_mailbox:?}"
    );
    let by_tag = d["by_tag"].as_array().expect("by_tag");
    assert_eq!(by_tag.clone(), vec![json!({"name": "bug", "count": 2})]);
    let by_agent = d["by_agent"].as_array().expect("by_agent");
    assert_eq!(
        by_agent.clone(),
        vec![
            json!({"name": "Ada Lovelace", "count": 2}),
            json!({"name": "Grace Hopper", "count": 1}),
        ],
        "assignee names resolved: {by_agent:?}"
    );
    let by_team = d["by_team"].as_array().expect("by_team");
    assert_eq!(
        by_team.clone(),
        vec![json!({"name": "Frontline", "count": 2})],
        "team via team_members of the assignee: {by_team:?}"
    );

    // Daily new + channel split.
    let daily_new = d["daily_new"].as_array().expect("daily_new");
    assert_eq!(
        daily_new.len(),
        1,
        "all in-range conversations share one date"
    );
    assert_eq!(daily_new[0]["value"], json!(5));
    let by_channel = d["by_channel"].as_array().expect("by_channel");
    assert_eq!(
        by_channel.clone(),
        vec![
            json!({"channel": "email", "count": 4}),
            json!({"channel": "chat", "count": 1})
        ],
        "by_channel groups via c.type: {by_channel:?}"
    );
    let channel_metrics = d["channel_metrics"].as_array().expect("channel_metrics");
    let email_metrics = channel_metrics
        .iter()
        .find(|r| r["channel"] == json!("email"))
        .expect("email channel_metrics row");
    assert_eq!(email_metrics["count"], json!(4));
    assert_eq!(email_metrics["first_response_avg_min"], json!(45));
    assert_eq!(email_metrics["resolution_avg_min"], json!(1440));

    // Mailbox comparison: one full KPI row per mailbox.
    let mailbox_comparison = d["mailbox_comparison"]
        .as_array()
        .expect("mailbox_comparison");
    assert_eq!(mailbox_comparison.len(), 2, "one row per mailbox");
    let billing = mailbox_comparison
        .iter()
        .find(|r| r["name"] == json!("Billing"))
        .expect("billing comparison row");
    assert_eq!(billing["new_conversations"], json!(3));
    assert_eq!(billing["active_conversations"], json!(2), "c4 + c7");
    assert_eq!(billing["closed_conversations"], json!(1), "c3");
    assert_eq!(billing["backlog"], json!(1), "c4");
    assert_eq!(billing["first_response_avg_min"], json!(60), "reply D only");
    assert_eq!(billing["great_ratings"], json!(0));
    assert_eq!(billing["total_ratings"], json!(1), "not-good on c3");
    let support = mailbox_comparison
        .iter()
        .find(|r| r["name"] == json!("Support"))
        .expect("support comparison row");
    assert_eq!(support["new_conversations"], json!(2), "c1 + c2");
    assert_eq!(support["great_ratings"], json!(1), "great on c1");
    assert_eq!(support["total_ratings"], json!(2), "great c1 + okay c2");

    // The daily snapshot cache landed in daily_metrics (upsertDailyMetric).
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let cached: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM daily_metrics WHERE metric_key = 'new_conversations'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        assert!(cached >= 1, "daily new cached for report snapshots");
    }

    // ── Dashboard: days=60 pulls the 40-day-old conversation in ─────────
    let resp = client
        .get(format!("{base}/api/analytics/dashboard?days=60"))
        .send()
        .await
        .expect("dashboard days=60");
    assert_eq!(resp.status().as_u16(), 200);
    let d: Value = resp.json().await.expect("dashboard body");
    assert_eq!(d["new_conversations"], json!(6), "c1-c5 + c7 in range");

    // ── Dashboard: mailboxIds scope ─────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/analytics/dashboard?mailboxIds=1"))
        .send()
        .await
        .expect("dashboard mailboxIds=1");
    assert_eq!(resp.status().as_u16(), 200);
    let d: Value = resp.json().await.expect("dashboard body");
    assert_eq!(d["new_conversations"], json!(2), "c1 + c2 in mailbox 1");
    assert_eq!(
        d["by_mailbox"].as_array().unwrap().len(),
        2,
        "by_mailbox is NOT mailbox-filtered (comparison view)"
    );
    assert_eq!(
        d["ratings"],
        json!({"great": 1, "okay": 1, "not-good": 0}),
        "ratings scoped through their linked conversation"
    );

    // ── Dashboard: channel scope ─────────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/analytics/dashboard?channel=email"))
        .send()
        .await
        .expect("dashboard channel=email");
    assert_eq!(resp.status().as_u16(), 200);
    let d: Value = resp.json().await.expect("dashboard body");
    assert_eq!(d["new_conversations"], json!(4), "c1, c3, c4, c7 are email");
    assert_eq!(
        d["by_channel"].as_array().unwrap().len(),
        2,
        "by_channel is NOT channel-filtered (it IS the breakdown)"
    );

    // ── Dashboard: 422 envelopes ────────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/analytics/dashboard?mailboxIds=abc"))
        .send()
        .await
        .expect("dashboard bad mailboxIds");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["error"], json!("ValidationError"));
    assert_eq!(
        body["message"],
        json!("mailboxIds must be a comma-separated list of positive integers.")
    );
    let resp = client
        .get(format!("{base}/api/analytics/dashboard?channel=phone"))
        .send()
        .await
        .expect("dashboard bad channel");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["message"], json!("channel must be 'email' or 'chat'."));

    // ════════════════════════════════════════════════════════════════════
    // AN-12: POST /api/reports/builder/run — MAIN metric semantics
    // ════════════════════════════════════════════════════════════════════
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let ten_days_ago = (chrono::Utc::now() - chrono::Duration::days(10))
        .format("%Y-%m-%d")
        .to_string();
    let run = |body: Value| {
        let client = client.clone();
        let url = format!("{base}/api/reports/builder/run");
        async move {
            client
                .post(url)
                .json(&body)
                .send()
                .await
                .expect("builder run")
        }
    };

    // agent_replies: only PUBLISHED, NOT-DELETED reply threads in range
    // (reply A on c1 + reply D on c3; the draft and the deleted reply do
    // not count; the 40-day-old reply is out of range).
    let resp = run(json!({
        "metric": "agent_replies", "dimension": "none",
        "dateFrom": ten_days_ago, "dateTo": today
    }))
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("builder body");
    assert_eq!(
        body["rows"][0]["value"],
        json!(2.0),
        "published+not-deleted replies only: {body:?}"
    );
    assert_eq!(body["metric"]["key"], json!("agent_replies"));
    assert!(body["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap_or("").starts_with("Metric definition:")));

    // customer_replies: published, not-deleted customer threads only
    // (c1 + c2 carry one each; the draft and deleted rows do not count).
    let resp = run(json!({
        "metric": "customer_replies", "dimension": "none",
        "dateFrom": ten_days_ago, "dateTo": today
    }))
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("builder body");
    assert_eq!(
        body["rows"][0]["value"],
        json!(2.0),
        "published+not-deleted customer messages only: {body:?}"
    );

    // The channel dimension groups via source_type.
    let resp = run(json!({
        "metric": "conversations", "dimension": "channel",
        "dateFrom": ten_days_ago, "dateTo": today
    }))
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("builder body");
    let rows = body["rows"].as_array().expect("rows");
    let find = |label: &str| {
        rows.iter()
            .find(|r| r["dimension_label"] == json!(label))
            .unwrap_or_else(|| panic!("no {label} row: {rows:?}"))["value"]
            .clone()
    };
    assert_eq!(find("email"), json!(5.0),
        "c1, c3, c4, c6, c7 have source_type=email (MAIN's conversations metric has no deleted filter)");
    assert_eq!(
        find("chat"),
        json!(1.0),
        "c2 has source_type=chat (c5 out of window)"
    );

    // The channel filter matches source_type.
    let resp = run(json!({
        "metric": "conversations", "dimension": "none",
        "dateFrom": ten_days_ago, "dateTo": today,
        "filters": { "channel": "chat" }
    }))
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("builder body");
    assert_eq!(
        body["rows"][0]["value"],
        json!(1.0),
        "only c2 is chat: {body:?}"
    );

    // The team dimension resolves through team_members of the assignee.
    let resp = run(json!({
        "metric": "conversations", "dimension": "team",
        "dateFrom": ten_days_ago, "dateTo": today
    }))
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("builder body");
    let rows = body["rows"].as_array().expect("rows");
    let find = |label: &str| {
        rows.iter()
            .find(|r| r["dimension_label"] == json!(label))
            .unwrap_or_else(|| panic!("no {label} row: {rows:?}"))["value"]
            .clone()
    };
    assert_eq!(
        find("Frontline"),
        json!(3.0),
        "Ada's c1 + c2 + c6 (c6 counted — no deleted filter)"
    );
    assert_eq!(
        find("(no team)"),
        json!(3.0),
        "c3 + c4 + c7 (Grace / unassigned)"
    );

    // Unknown metric -> the reference 422 with the enum error.
    let resp = run(json!({
        "metric": "not_a_metric", "dimension": "none",
        "dateFrom": ten_days_ago, "dateTo": today
    }))
    .await;
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("builder 422 body");
    assert_eq!(body["error"], json!("ValidationError"));

    // ════════════════════════════════════════════════════════════════════
    // AC-04: GET /api/conversations/:id/events — the full timeline
    // ════════════════════════════════════════════════════════════════════
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let ev = |event_type: &str,
                  actor_type: &str,
                  actor_id: Option<i64>,
                  occurred: &str,
                  metadata: Value,
                  thread: Option<i64>| {
            spp_core::activity::record_full_event(
                &conn,
                &spp_core::activity::FullActivityEvent {
                    base: spp_core::activity::ActivityEvent {
                        id: None,
                        conversation_id: c1,
                        event_type: event_type.to_string(),
                        actor_type: actor_type.to_string(),
                        actor_id,
                        occurred_at: occurred.to_string(),
                        dedup_key: format!("test:{event_type}:{occurred}:{actor_type}"),
                    },
                    thread_local_id: thread,
                    source: "sync".to_string(),
                    metadata: Some(metadata.to_string()),
                },
            )
            .unwrap();
        };
        ev(
            "customer_message",
            "customer",
            Some(100),
            "2026-01-01T10:00:00Z",
            json!({"thread_remote_id": 900, "thread_type": "customer"}),
            None,
        );
        ev(
            "human_agent_message",
            "user",
            Some(10),
            "2026-01-01T11:00:00Z",
            json!({"thread_remote_id": 901}),
            Some(31),
        );
        ev(
            "status_changed",
            "system_user",
            Some(7),
            "2026-01-01T12:00:00Z",
            json!({"from": "active", "to": "pending"}),
            None,
        );
        ev(
            "lineitem_action",
            "unknown",
            None,
            "2026-01-01T13:00:00Z",
            json!({"text": "moved"}),
            None,
        );
    }

    // Full timeline with resolved actor names + counts.
    let resp = client
        .get(format!("{base}/api/conversations/{c1}/events"))
        .send()
        .await
        .expect("events");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("events body");
    assert_eq!(body["conversation_id"], json!(c1));
    let events = body["events"].as_array().expect("events array");
    assert_eq!(events.len(), 4, "all four events: {events:?}");
    // Chronological order (occurred_at ASC, id ASC).
    assert_eq!(events[0]["event_type"], json!("customer_message"));
    assert_eq!(
        events[0]["actor_name"],
        json!("Carol Client"),
        "customer actor name resolved: {events:?}"
    );
    assert_eq!(events[0]["actor_local_id"], json!(100));
    assert_eq!(events[0]["metadata"]["thread_remote_id"], json!(900));
    assert_eq!(events[0]["source"], json!("sync"));
    assert_eq!(
        events[1]["actor_name"],
        json!("Ada Lovelace"),
        "user actor name resolved"
    );
    assert_eq!(events[1]["thread_local_id"], json!(31));
    assert_eq!(
        events[2]["actor_name"],
        json!("Sibyl System"),
        "system_user actor name resolved"
    );
    assert_eq!(
        events[3]["actor_name"],
        Value::Null,
        "unknown actors carry no name"
    );
    // Every event row carries the full reference field set.
    for e in events {
        for key in [
            "id",
            "conversation_id",
            "thread_local_id",
            "event_type",
            "actor_type",
            "actor_local_id",
            "occurred_at",
            "source",
            "metadata",
            "created_at",
            "actor_name",
        ] {
            assert!(e.get(key).is_some(), "event field missing: {key} in {e:?}");
        }
    }
    // Counts summarize per type (all events, not just the limited page).
    assert_eq!(
        body["counts"],
        json!({
            "customer_message": 1,
            "human_agent_message": 1,
            "status_changed": 1,
            "lineitem_action": 1,
        })
    );

    // limit caps the timeline but not the counts.
    let resp = client
        .get(format!("{base}/api/conversations/{c1}/events?limit=2"))
        .send()
        .await
        .expect("events limit=2");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("events body");
    assert_eq!(body["events"].as_array().unwrap().len(), 2);
    assert_eq!(body["counts"]["customer_message"], json!(1));
    assert_eq!(body["counts"].as_object().unwrap().len(), 4);

    // Garbage limit falls back to 200; oversized clamps to 1000 (no error).
    let resp = client
        .get(format!("{base}/api/conversations/{c1}/events?limit=abc"))
        .send()
        .await
        .expect("events limit=abc");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("events body");
    assert_eq!(
        body["events"].as_array().unwrap().len(),
        4,
        "fallback 200 > 4"
    );
    let resp = client
        .get(format!("{base}/api/conversations/{c1}/events?limit=99999"))
        .send()
        .await
        .expect("events limit=99999");
    assert_eq!(resp.status().as_u16(), 200);

    // Unknown conversation -> the reference 404 envelope.
    let resp = client
        .get(format!("{base}/api/conversations/99999/events"))
        .send()
        .await
        .expect("events unknown");
    assert_eq!(resp.status().as_u16(), 404);
    let body: Value = resp.json().await.expect("404 body");
    assert_eq!(body["error"], json!("NotFound"));
    assert_eq!(body["message"], json!("Conversation not found locally."));
}
