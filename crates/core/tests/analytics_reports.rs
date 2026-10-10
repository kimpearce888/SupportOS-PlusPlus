//! AN-06/AN-07/AN-08/AN-10/AN-11 verification: the M3 analytics report batch
//! on the real HTTP server.
//!
//! - GET /api/reports/doc-gaps — `analyticsService.docGaps(days)`: repeated
//!   questions with knowledge-coverage classification (missing/partial/
//!   ambiguous via FTS hits), the first published reply as the known
//!   answer, and a suggested doc title.
//! - GET /api/reports/answer-reuse — `analyticsService.answerReuse(days)`:
//!   `{candidates}` with common_resolution, saved_reply_name and
//!   knowledge_doc_title lookups (recommendation only).
//! - GET /api/reports/issue-radar — `analyticsService.issueRadar()`: the
//!   reference alert vocabulary (new_cluster, volume_spike, recurring_issue,
//!   high_volume_question, escalation_heavy, rating_correlated,
//!   reappearing_issue, customer_concentration, inbox_concentration,
//!   release_correlation, repeated_unresolved), each alert carrying
//!   conversation links and association-only wording.
//! - GET /api/reports/metric-definitions — the 11 seeded metric definitions,
//!   ordered by key, limitations defaulted to "".
//! - POST /api/reports/release-events — zod-shaped validation (name/version/
//!   occurredAt/notes caps, ISO regex) with the reference 422 envelope, and
//!   the write itself.
//! - GET /api/reports/release-correlation — 7-day before/after windows per
//!   release event, `{releases, note, source: 'local'}`.
//! - GET /api/reports/helpscout/:reportKey — the four provider-proxied
//!   Help Scout native reports (labeled source helpscout) and the 404
//!   envelope for unknown keys.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use spp_core::helpscout::HelpScoutProvider;

const SERVER_PORT: u16 = 4010;

fn seed_run(
    conn: &rusqlite::Connection,
    hash: &str,
    conversation_id: i64,
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

/// Insert a conversation; `created_at_sql` is a SQLite expression evaluated
/// by the engine (e.g. `datetime('now', '-3 days')`).
fn insert_conv(
    conn: &rusqlite::Connection,
    remote: i64,
    mailbox: i64,
    customer: i64,
    created_at_sql: &str,
) -> i64 {
    conn.execute(
        &format!(
            "INSERT INTO conversations (remote_id, number, mailbox_local_id, customer_local_id, status, created_at, updated_at)
             VALUES ({remote}, {remote}, {mailbox}, {customer}, 'closed', {created_at_sql}, {created_at_sql})"
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

fn seed_reply(conn: &rusqlite::Connection, conversation_id: i64, body: &str) {
    conn.execute(
        "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at)
         VALUES (?1, 'reply', ?2, 'user', datetime('now'))",
        rusqlite::params![conversation_id, body],
    )
    .unwrap();
}

#[tokio::test]
async fn analytics_reports_batch() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");
    spp_core::ai_pipeline::ensure_pipeline_schema(&conn).expect("pipeline schema");

    conn.execute(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (2, 202, 'Billing')",
        [],
    )
    .unwrap();
    // DB-03: M047 FKs (foreign_keys=ON) — the customer ids insert_conv keys on.
    conn.execute(
        "INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES
            (100, 2100, 'C100'), (101, 2101, 'C101'), (102, 2102, 'C102'),
            (103, 2103, 'C103'), (104, 2104, 'C104'), (105, 2105, 'C105'),
            (110, 2110, 'C110'), (120, 2120, 'C120'), (121, 2121, 'C121'),
            (122, 2122, 'C122'), (123, 2123, 'C123'), (124, 2124, 'C124'),
            (125, 2125, 'C125'), (126, 2126, 'C126'), (127, 2127, 'C127'),
            (128, 2128, 'C128')",
        [],
    )
    .unwrap();

    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // ── Cluster A "API sync failures": 7 conversations (6 in the last 6
    //    days + one 20 days ago from the same customer) from 3 customers,
    //    all in mailbox 1, with 2 not-good ratings and an incident release
    //    inside the burst window. Trend: recent=6, previous=1 -> rising.
    let mut a_ids = Vec::new();
    for (i, days) in [1i64, 2, 3, 4, 5, 6].iter().enumerate() {
        let customer = [100i64, 100, 101, 101, 102, 100][i];
        a_ids.push(insert_conv(
            &conn,
            100 + *days,
            1,
            customer,
            &format!("datetime('now', '-{days} days')"),
        ));
    }
    let a_old = insert_conv(&conn, 199, 1, 100, "datetime('now', '-20 days')");
    conn.execute(
        "INSERT INTO issue_clusters (id, name, conversation_count) VALUES (1, 'API sync failures', 7)",
        [],
    )
    .unwrap();
    for id in &a_ids {
        conn.execute(
            "INSERT INTO issue_cluster_conversations (cluster_id, conversation_id) VALUES (1, ?1)",
            rusqlite::params![id],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO issue_cluster_conversations (cluster_id, conversation_id) VALUES (1, ?1)",
        rusqlite::params![a_old],
    )
    .unwrap();
    // Two not-good ratings on cluster A conversations.
    for (i, id) in a_ids.iter().take(2).enumerate() {
        conn.execute(
            "INSERT INTO ratings (remote_id, conversation_id, rating, remote_created_at)
             VALUES (?1, ?2, 'not-good', datetime('now'))",
            rusqlite::params![900 + i as i64, id],
        )
        .unwrap();
    }
    // An incident release inside the burst window (3 days ago).
    conn.execute(
        "INSERT INTO incidents (id, status, severity, source) VALUES (1, 'resolved', 'sev3', 'manual')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO incident_releases (incident_id, version_label, released_at)
         VALUES (1, 'v2.1.0', datetime('now', '-3 days'))",
        [],
    )
    .unwrap();

    // ── Cluster B "Fresh complaint": 2 conversations 2 days ago -> trend
    //    new -> new_cluster (warning: < 5 conversations).
    conn.execute(
        "INSERT INTO issue_clusters (id, name, conversation_count) VALUES (2, 'Fresh complaint', 2)",
        [],
    )
    .unwrap();
    for remote in [300i64, 301] {
        let id = insert_conv(&conn, remote, 1, 110, "datetime('now', '-2 days')");
        conn.execute(
            "INSERT INTO issue_cluster_conversations (cluster_id, conversation_id) VALUES (2, ?1)",
            rusqlite::params![id],
        )
        .unwrap();
    }

    // ── Cluster C "Legacy problem": 6 conversations 40-45 days ago ->
    //    trend stable -> recurring_issue. 3 distinct customers (no
    //    concentration); split mailboxes (no inbox concentration).
    conn.execute(
        "INSERT INTO issue_clusters (id, name, conversation_count) VALUES (3, 'Legacy problem', 6)",
        [],
    )
    .unwrap();
    for (i, days) in [40i64, 41, 42, 43, 44, 45].iter().enumerate() {
        let customer = [100i64, 101, 102, 103, 104, 105][i];
        let mailbox = if i % 2 == 0 { 1 } else { 2 };
        let id = insert_conv(
            &conn,
            400 + *days,
            mailbox,
            customer,
            &format!("datetime('now', '-{days} days')"),
        );
        conn.execute(
            "INSERT INTO issue_cluster_conversations (cluster_id, conversation_id) VALUES (3, ?1)",
            rusqlite::params![id],
        )
        .unwrap();
    }

    // ── Cluster D "Reappearing bug": 2 conversations 40 days ago + 2
    //    three days ago, 3 customers, split mailboxes -> reappearing_issue
    //    (and repeated_unresolved for the shared customer).
    conn.execute(
        "INSERT INTO issue_clusters (id, name, conversation_count) VALUES (4, 'Reappearing bug', 4)",
        [],
    )
    .unwrap();
    for (remote, days, customer, mailbox) in [
        (500i64, 40, 100, 1),
        (501, 41, 101, 2),
        (502, 3, 102, 1),
        (503, 2, 100, 2),
    ] {
        let id = insert_conv(
            &conn,
            remote,
            mailbox,
            customer,
            &format!("datetime('now', '-{days} days')"),
        );
        conn.execute(
            "INSERT INTO issue_cluster_conversations (cluster_id, conversation_id) VALUES (4, ?1)",
            rusqlite::params![id],
        )
        .unwrap();
    }

    // ── Question conversations (ticket analyses) ──────────────────────────
    // QA "how do I export my data?" x3 -> high_volume_question + doc gap
    // (missing) + answer reuse candidate.
    let mut qa_ids = Vec::new();
    for remote in [600i64, 601, 602] {
        let id = insert_conv(&conn, remote, 1, 120, "datetime('now')");
        seed_run(
            &conn,
            &format!("qa{remote}"),
            id,
            "completed",
            &now,
            json!({ "primary_question": "how do I export my data?" }),
        );
        qa_ids.push(id);
    }
    seed_reply(&conn, qa_ids[0], "You can export from Settings > Export.");
    // QB "billing address change procedures" x2 -> doc gap (partial).
    for remote in [610i64, 611] {
        let id = insert_conv(&conn, remote, 2, 121, "datetime('now')");
        seed_run(
            &conn,
            &format!("qb{remote}"),
            id,
            "completed",
            &now,
            json!({ "primary_question": "billing address change procedures" }),
        );
    }
    // QC "password reset help please" x2 -> doc gap (ambiguous).
    for remote in [620i64, 621] {
        let id = insert_conv(&conn, remote, 1, 122, "datetime('now')");
        seed_run(
            &conn,
            &format!("qc{remote}"),
            id,
            "completed",
            &now,
            json!({ "primary_question": "password reset help please" }),
        );
    }
    // A one-off question: never a gap nor a reuse candidate.
    {
        let id = insert_conv(&conn, 630, 1, 123, "datetime('now')");
        seed_run(
            &conn,
            "qsingle",
            id,
            "completed",
            &now,
            json!({ "primary_question": "one off curiosity" }),
        );
    }
    // QD "refund policy timing" x2 -> answer reuse candidate with a saved
    // reply + knowledge document match.
    let mut qd_ids = Vec::new();
    for remote in [640i64, 641] {
        let id = insert_conv(&conn, remote, 2, 124, "datetime('now')");
        seed_run(
            &conn,
            &format!("qd{remote}"),
            id,
            "completed",
            &now,
            json!({ "primary_question": "refund policy timing" }),
        );
        qd_ids.push(id);
    }
    seed_reply(&conn, qd_ids[0], "Send the refund policy timing document.");

    // ── Knowledge coverage fixtures (fts_knowledge) ───────────────────────
    conn.execute(
        "INSERT INTO fts_knowledge (title, content, chunk_id, document_id, visibility)
         VALUES ('Billing address change procedures', 'guide', 1, 1, 'internal_only')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO fts_knowledge (title, content, chunk_id, document_id, visibility)
         VALUES ('Password reset help', 'please visit the reset page', 2, 2, 'internal_only')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO fts_knowledge (title, content, chunk_id, document_id, visibility)
         VALUES ('Reset password please help', 'x', 3, 3, 'internal_only')",
        [],
    )
    .unwrap();

    // ── Answer reuse fixtures: a saved reply + a knowledge document ──────
    conn.execute(
        "INSERT INTO saved_replies (remote_id, name, preview) VALUES (1, 'refund policy timing - macro', 'sends the policy')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO knowledge_sources (id, name) VALUES (1, 'test source')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO knowledge_documents (source_id, title, content)
         VALUES (1, 'refund policy timing FAQ', 'see the policy')",
        [],
    )
    .unwrap();

    // ── Escalation fixture: 2 recent conversations tagged 'escalated' ──
    conn.execute(
        "INSERT INTO tags (remote_id, name) VALUES (50, 'escalated')",
        [],
    )
    .unwrap();
    for remote in [700i64, 701] {
        let id = insert_conv(&conn, remote, 1, 125, "datetime('now', '-2 days')");
        conn.execute(
            "INSERT INTO conversation_tags (conversation_id, tag_id)
             SELECT ?1, id FROM tags WHERE remote_id = 50",
            rusqlite::params![id],
        )
        .unwrap();
    }

    // ── Global volume fixtures: 3 conversations 10 days ago (prev window)
    //    plus everything above in the recent window (>= 10, >= 1.4x).
    for remote in [710i64, 711, 712] {
        insert_conv(&conn, remote, 1, 126, "datetime('now', '-10 days')");
    }

    // ── Release correlation fixtures (months old; outside every radar
    //    window): 2 conversations before, 3 after the summer release.
    for remote in [800i64, 801] {
        insert_conv(&conn, remote, 1, 127, "'2026-05-30 10:00:00'");
    }
    for remote in [810i64, 811, 812] {
        insert_conv(&conn, remote, 1, 128, "'2026-06-03 10:00:00'");
    }

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

    // ── GET /api/reports/doc-gaps ────────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/reports/doc-gaps"))
        .send()
        .await
        .expect("doc-gaps");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("doc-gaps body");
    let gaps = body["gaps"].as_array().expect("gaps array");
    let find_gap = |q: &str| {
        gaps.iter()
            .find(|g| g["question"].as_str() == Some(q))
            .unwrap_or_else(|| panic!("no gap for {q}: {gaps:?}"))
            .clone()
    };
    let qa_gap = find_gap("how do i export my data?");
    assert_eq!(qa_gap["conversation_count"].as_i64(), Some(3));
    assert_eq!(qa_gap["coverage"].as_str(), Some("missing"));
    assert_eq!(
        qa_gap["known_answer"].as_str(),
        Some("You can export from Settings > Export.")
    );
    assert_eq!(
        qa_gap["suggested_doc_title"].as_str(),
        Some("Documentation: how do i export my data?")
    );
    let qb_gap = find_gap("billing address change procedures");
    assert_eq!(qb_gap["coverage"].as_str(), Some("partial"));
    let qc_gap = find_gap("password reset help please");
    assert_eq!(qc_gap["coverage"].as_str(), Some("ambiguous"));
    assert!(
        gaps.iter()
            .all(|g| g["question"].as_str() != Some("one off curiosity")),
        "single-conversation questions are never gaps"
    );

    // ── GET /api/reports/answer-reuse ────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/reports/answer-reuse"))
        .send()
        .await
        .expect("answer-reuse");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("answer-reuse body");
    let candidates = body["candidates"].as_array().expect("candidates array");
    let find_candidate = |q: &str| {
        candidates
            .iter()
            .find(|c| c["question"].as_str() == Some(q))
            .unwrap_or_else(|| panic!("no candidate for {q}: {candidates:?}"))
            .clone()
    };
    let qd = find_candidate("refund policy timing");
    assert_eq!(qd["conversation_count"].as_i64(), Some(2));
    assert_eq!(
        qd["common_resolution"].as_str(),
        Some("Send the refund policy timing document.")
    );
    assert_eq!(
        qd["saved_reply_name"].as_str(),
        Some("refund policy timing - macro")
    );
    assert_eq!(
        qd["knowledge_doc_title"].as_str(),
        Some("refund policy timing FAQ")
    );
    let qa = find_candidate("how do i export my data?");
    assert_eq!(qa["saved_reply_name"].as_str(), None);
    assert_eq!(qa["knowledge_doc_title"].as_str(), None);
    assert_eq!(qa["conversation_count"].as_i64(), Some(3));
    assert!(
        candidates
            .iter()
            .all(|c| c["question"].as_str() != Some("one off curiosity")),
        "single-conversation questions are never reuse candidates"
    );

    // ── GET /api/reports/issue-radar ────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/reports/issue-radar"))
        .send()
        .await
        .expect("issue-radar");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("issue-radar body");
    let alerts = body["alerts"].as_array().expect("alerts array");
    let kinds = alerts
        .iter()
        .filter_map(|a| a["kind"].as_str())
        .collect::<Vec<&str>>();
    for expected in [
        "new_cluster",
        "volume_spike",
        "recurring_issue",
        "high_volume_question",
        "escalation_heavy",
        "rating_correlated",
        "reappearing_issue",
        "customer_concentration",
        "inbox_concentration",
        "release_correlation",
        "repeated_unresolved",
    ] {
        assert!(
            kinds.contains(&expected),
            "radar must emit {expected}; got {kinds:?}"
        );
    }
    for a in alerts {
        for field in [
            "kind",
            "title",
            "detail",
            "conversation_ids",
            "cluster_id",
            "severity",
        ] {
            assert!(a.get(field).is_some(), "alert missing {field}: {a:?}");
        }
        assert!(
            a["conversation_ids"].as_array().map(|v| v.len()) <= Some(10),
            "conversation links capped at 10"
        );
    }
    // The release-correlated cluster mentions the incident release as an
    // association (never causation).
    let release_corr = alerts
        .iter()
        .find(|a| a["kind"] == "release_correlation")
        .expect("release_correlation alert");
    assert!(release_corr["detail"].as_str().unwrap().contains("v2.1.0"));
    assert!(release_corr["detail"]
        .as_str()
        .unwrap()
        .contains("association"));
    // The global volume spike carries no cluster id and association wording.
    let global_spike = alerts
        .iter()
        .find(|a| a["kind"] == "volume_spike" && a["cluster_id"].is_null())
        .expect("global volume_spike alert");
    assert!(global_spike["detail"]
        .as_str()
        .unwrap()
        .contains("not a causal claim"));
    // The rating correlation never claims proof.
    let rating_corr = alerts
        .iter()
        .find(|a| a["kind"] == "rating_correlated")
        .expect("rating_correlated alert");
    assert!(rating_corr["detail"]
        .as_str()
        .unwrap()
        .contains("association"));

    // ── GET /api/reports/metric-definitions ──────────────────────────────
    let resp = client
        .get(format!("{base}/api/reports/metric-definitions"))
        .send()
        .await
        .expect("metric-definitions");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("metric-definitions body");
    let definitions = body["definitions"].as_array().expect("definitions array");
    assert_eq!(definitions.len(), 11, "reference 003 seeds exactly 11");
    let keys: Vec<&str> = definitions
        .iter()
        .filter_map(|d| d["key"].as_str())
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "definitions ordered by key");
    // The exact reference key set (migration 003), in key order.
    assert_eq!(
        keys,
        [
            "active_conversations",
            "ai_draft_acceptance",
            "backlog",
            "closed_conversations",
            "first_response_time_local",
            "new_conversations",
            "pending_conversations",
            "ratings",
            "replies_sent",
            "resolution_time_local",
            "unassigned",
        ],
        "AN-09: the 11 seeded metric definitions match the reference key set"
    );
    assert_eq!(keys.first(), Some(&"active_conversations"));
    for d in definitions {
        assert!(d["limitations"].is_string(), "limitations defaulted: {d:?}");
        assert!(d["name"].is_string());
        assert!(d["source"].is_string());
    }
    let new_conversations = definitions
        .iter()
        .find(|d| d["key"] == "new_conversations")
        .unwrap();
    assert_eq!(new_conversations["source"].as_str(), Some("local"));
    assert!(new_conversations["formula"].is_string());

    // ── POST /api/reports/release-events (happy path) ────────────────────
    let resp = client
        .post(format!("{base}/api/reports/release-events"))
        .json(&json!({
            "name": "Summer release",
            "version": "2.4.0",
            "occurredAt": "2026-06-01T10:00:00Z",
            "notes": "payments rewrite"
        }))
        .send()
        .await
        .expect("release-events post");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("release-events body");
    assert_eq!(body["ok"].as_bool(), Some(true));
    assert_eq!(body["message"].as_str(), Some("Release event recorded."));

    // ── GET /api/reports/release-correlation ─────────────────────────────
    let resp = client
        .get(format!("{base}/api/reports/release-correlation"))
        .send()
        .await
        .expect("release-correlation");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("release-correlation body");
    assert_eq!(body["source"].as_str(), Some("local"));
    assert!(
        body["note"]
            .as_str()
            .unwrap()
            .contains("potentially related"),
        "wording stays association-only"
    );
    let releases = body["releases"].as_array().expect("releases array");
    assert_eq!(releases.len(), 1);
    let r = &releases[0];
    assert_eq!(r["release"].as_str(), Some("Summer release"));
    assert_eq!(r["version"].as_str(), Some("2.4.0"));
    assert_eq!(r["occurred_at"].as_str(), Some("2026-06-01T10:00:00Z"));
    assert_eq!(r["before_7d"].as_i64(), Some(2));
    assert_eq!(r["after_7d"].as_i64(), Some(3));

    // ── POST /api/reports/release-events (zod 422s) ──────────────────────
    let post_bad = |payload: Value| {
        let client = reqwest::Client::new();
        let base = base.clone();
        async move {
            let resp = client
                .post(format!("{base}/api/reports/release-events"))
                .json(&payload)
                .send()
                .await
                .expect("post");
            let status = resp.status().as_u16();
            let body: Value = resp.json().await.expect("body");
            (status, body)
        }
    };
    let (status, body) = post_bad(json!({})).await;
    assert_eq!(status, 422);
    assert_eq!(body["error"].as_str(), Some("ValidationError"));
    assert_eq!(
        body["message"].as_str(),
        Some("Invalid request (name): Required")
    );
    let (status, body) = post_bad(json!({ "name": "" })).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"].as_str(),
        Some("Invalid request (name): String must contain at least 1 character(s)")
    );
    let (status, body) = post_bad(json!({ "name": "x".repeat(201) })).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"].as_str(),
        Some("Invalid request (name): String must contain at most 200 character(s)")
    );
    let (status, body) = post_bad(json!({
        "name": "ok",
        "occurredAt": "2026/01/01"
    }))
    .await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"].as_str(),
        Some("Invalid request (occurredAt): Invalid")
    );
    let (status, _body) = post_bad(json!({
        "name": "ok",
        "occurredAt": "2026-01-01TZ"
    }))
    .await;
    assert_eq!(status, 422);
    let (status, body) = post_bad(json!({
        "name": "ok",
        "occurredAt": "2026-01-01T10:00:00Z",
        "version": "v".repeat(101)
    }))
    .await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"].as_str(),
        Some("Invalid request (version): String must contain at most 100 character(s)")
    );
    let (status, body) = post_bad(json!({
        "name": "ok",
        "occurredAt": "2026-01-01",
        "notes": "y".repeat(2001)
    }))
    .await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"].as_str(),
        Some("Invalid request (notes): String must contain at most 2000 character(s)")
    );

    // ── GET /api/reports/helpscout/:reportKey (fake provider world) ─────
    // Expected numbers come from a second, identical demo provider.
    let reference = spp_core::helpscout::FakeHelpScoutProvider::new_demo();
    let (start, end) = ("2000-01-01".to_string(), "2100-01-01".to_string());

    let resp = client
        .get(format!(
            "{base}/api/reports/helpscout/company?from=2000-01-01&to=2100-01-01"
        ))
        .send()
        .await
        .expect("helpscout company");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("helpscout company body");
    assert_eq!(body["ok"].as_bool(), Some(true));
    assert_eq!(body["source"].as_str(), Some("helpscout"));
    assert!(
        body["note"]
            .as_str()
            .unwrap()
            .contains("Help Scout native reporting"),
        "origin labeling"
    );
    let expected = reference
        .get_company_overall_report(&start, &end)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body["report"], serde_json::to_value(&expected).unwrap());

    let resp = client
        .get(format!(
            "{base}/api/reports/helpscout/conversations?from=2000-01-01&to=2100-01-01"
        ))
        .send()
        .await
        .expect("helpscout conversations");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("helpscout conversations body");
    let expected = reference
        .get_conversations_overall_report(&start, &end)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body["report"], serde_json::to_value(&expected).unwrap());
    assert!(
        body["report"]["data"]["byStatus"].is_object(),
        "byStatus breakdown"
    );

    let resp = client
        .get(format!(
            "{base}/api/reports/helpscout/happiness?from=2000-01-01&to=2100-01-01"
        ))
        .send()
        .await
        .expect("helpscout happiness");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("helpscout happiness body");
    let expected = reference
        .get_happiness_ratings_report(&start, &end)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body["report"], serde_json::to_value(&expected).unwrap());

    let resp = client
        .get(format!(
            "{base}/api/reports/helpscout/productivity?from=2000-01-01&to=2100-01-01"
        ))
        .send()
        .await
        .expect("helpscout productivity");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("helpscout productivity body");
    let expected = reference
        .get_productivity_overall_report(&start, &end)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body["report"], serde_json::to_value(&expected).unwrap());

    // Default days=30 window (no from/to): same shape, provider-sourced.
    let resp = client
        .get(format!("{base}/api/reports/helpscout/company"))
        .send()
        .await
        .expect("helpscout company default");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("helpscout company default body");
    assert_eq!(body["ok"].as_bool(), Some(true));
    assert_eq!(body["report"]["key"].as_str(), Some("hs_company_overall"));
    assert_eq!(body["report"]["source"].as_str(), Some("helpscout"));

    // Unknown keys are the reference 404 envelope.
    let resp = client
        .get(format!("{base}/api/reports/helpscout/unknown"))
        .send()
        .await
        .expect("helpscout unknown");
    assert_eq!(resp.status().as_u16(), 404);
    let body: Value = resp.json().await.expect("helpscout unknown body");
    assert_eq!(body["statusCode"].as_i64(), Some(404));
    assert_eq!(body["error"].as_str(), Some("NotFound"));
    assert_eq!(
        body["message"].as_str(),
        Some("Unknown report. Available: company, conversations, happiness, productivity.")
    );
}
