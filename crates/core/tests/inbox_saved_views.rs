//! VW-03 verification: saved views + AI-attribute filters compiled into the
//! inbox list query on the real HTTP server (reference conversations.ts:
//! 24-130 — page clamps, savedViewId compilation through the SAME view
//! engine the preview uses, and the live aiAttribute filter).
//!
//! - `?savedViewId=N` compiles the stored condition tree at OPEN time
//!   (tags + status), serving the `Saved view '...' (vN) applied.` note;
//!   unknown ids answer the reference 404 envelope, non-numeric ids the
//!   filter-schema 422.
//! - `?aiAttribute=key&aiAttrOp=op&aiAttrValue=v` filters on the CURRENT
//!   (non-superseded) ai_attributes rows — including the honest `unknown`
//!   value and ordered-enum comparisons (gt/gte/lt/lte) — composable via
//!   AND with the saved-view fragment.
//! - The page/pageSize clamps (1..100000 / 1..100) and the actual page
//!   echo in the response envelope.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4020;

#[tokio::test]
async fn inbox_saved_views_and_ai_filters() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // Four conversations across two mailboxes with distinct updated_at
    // (the list orders by newest activity): billing-tagged actives with AI
    // attributes, a closed conversation whose intent row is SUPERSEDED, and
    // a bare active one with no attributes.
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support'), (2, 12, 'Billing');
         INSERT INTO users (id, remote_id, first_name, last_name) VALUES (7, 70, 'Dana', 'Reyes');
         INSERT INTO customers (id, remote_id, first_name, email) VALUES (11, 110, 'Ada', 'ada@example.com');
         INSERT INTO customers (id, remote_id, first_name, email) VALUES (12, 120, 'Belle', 'belle@example.com');
         INSERT INTO customers (id, remote_id, first_name, email) VALUES (13, 130, 'Carlos', 'carlos@example.com');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, assignee_id, updated_at)
             VALUES (1, 101, 101, 'Billing bug', 'active', 1, 11, 7, '2026-10-05 10:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, assignee_id, updated_at)
             VALUES (2, 102, 102, 'Billing question', 'active', 1, 12, 7, '2026-10-06 11:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, updated_at)
             VALUES (3, 103, 103, 'Old export issue', 'closed', 2, 13, '2026-10-04 09:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, updated_at)
             VALUES (4, 104, 104, 'Bare active', 'active', 2, 11, '2026-10-03 08:00:00');
         INSERT INTO tags (id, remote_id, name) VALUES (21, 210, 'billing'), (22, 220, 'export');
         INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (1, 21), (2, 21), (2, 22), (3, 22);
         INSERT INTO ai_attributes (conversation_id, attribute, value, value_type, confidence, source, schema_version)
             VALUES (1, 'intent', 'bug_report', 'enum', 'high', 'deterministic', '3.0');
         INSERT INTO ai_attributes (conversation_id, attribute, value, value_type, confidence, source, schema_version)
             VALUES (2, 'intent', 'question', 'enum', 'high', 'deterministic', '3.0');
         INSERT INTO ai_attributes (conversation_id, attribute, value, value_type, confidence, source, schema_version)
             VALUES (2, 'urgency', 'high', 'enum', 'high', 'deterministic', '3.0');
         INSERT INTO ai_attributes (conversation_id, attribute, value, value_type, confidence, source, schema_version, superseded_at)
             VALUES (3, 'intent', 'question', 'enum', 'high', 'deterministic', '3.0', '2026-10-02 00:00:00');",
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

    // ── Create a saved view through the API ────────────────────────────
    let r = client
        .post(format!("{base}/api/inbox-views"))
        .json(&json!({
            "name": "Billing active",
            "definition": {
                "combinator": "all",
                "conditions": [
                    {"kind": "tags", "tags": ["billing"], "mode": "any"},
                    {"kind": "status", "statuses": ["active"]}
                ]
            }
        }))
        .send()
        .await
        .expect("create view");
    assert_eq!(r.status().as_u16(), 200, "view create should succeed");
    let body: Value = r.json().await.expect("body");
    let view_id = body["view"]["id"].as_i64().expect("view id");

    // ── ?savedViewId — the compiled condition tree applies ────────────
    let body: Value = client
        .get(format!("{base}/api/conversations?savedViewId={view_id}"))
        .send()
        .await
        .expect("saved view list")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(2));
    let conversations = body["conversations"].as_array().expect("conversations");
    assert_eq!(conversations.len(), 2);
    // Newest activity first: conv 2 (10-06), then conv 1 (10-05).
    assert_eq!(conversations[0]["id"], json!(2));
    assert_eq!(conversations[1]["id"], json!(1));
    let notes = body["notes"].as_array().expect("notes");
    assert_eq!(notes[0], json!("Saved view 'Billing active' (v1) applied."));

    // ── composition: saved view AND ai attribute ──────────────────────
    let body: Value = client
        .get(format!(
            "{base}/api/conversations?savedViewId={view_id}&aiAttribute=intent&aiAttrValue=bug_report"
        ))
        .send()
        .await
        .expect("composed list")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["conversations"][0]["id"], json!(1));
    let notes = body["notes"].as_array().expect("notes");
    assert_eq!(
        notes[1],
        json!("AI attribute filter: intent equals \"bug_report\" (local layer; missing values read as unknown).")
    );

    // ── the ai attribute filter alone ──────────────────────────────────
    let body: Value = client
        .get(format!(
            "{base}/api/conversations?aiAttribute=urgency&aiAttrValue=high"
        ))
        .send()
        .await
        .expect("urgency filter")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["conversations"][0]["id"], json!(2));

    // `unknown` finds tickets WITHOUT a current value (the superseded row
    // on conv 3 does not count as a value).
    let body: Value = client
        .get(format!(
            "{base}/api/conversations?aiAttribute=intent&aiAttrValue=unknown"
        ))
        .send()
        .await
        .expect("unknown filter")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(2));
    let ids: Vec<i64> = body["conversations"]
        .as_array()
        .expect("conversations")
        .iter()
        .map(|c| c["id"].as_i64().unwrap())
        .collect();
    assert!(ids.contains(&3) && ids.contains(&4), "ids: {ids:?}");

    // Ordered-enum comparison: urgency gt moderate -> high.
    let body: Value = client
        .get(format!(
            "{base}/api/conversations?aiAttribute=urgency&aiAttrOp=gt&aiAttrValue=moderate"
        ))
        .send()
        .await
        .expect("enum comparison")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["conversations"][0]["id"], json!(2));

    // ── mailboxId + page clamps ────────────────────────────────────────
    let body: Value = client
        .get(format!("{base}/api/conversations?mailboxId=2"))
        .send()
        .await
        .expect("mailbox filter")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(2));
    assert!(body["conversations"][0]["id"] == json!(3));

    // Actives newest-first: 2, 1, 4 — page 2 of pageSize 2 serves [4].
    let body: Value = client
        .get(format!(
            "{base}/api/conversations?view=active&pageSize=2&page=2"
        ))
        .send()
        .await
        .expect("page 2")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total"], json!(3));
    assert_eq!(body["page"], json!(2));
    assert_eq!(body["page_size"], json!(2));
    let ids: Vec<i64> = body["conversations"]
        .as_array()
        .expect("conversations")
        .iter()
        .map(|c| c["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![4]);

    // pageSize beyond the v1.6.0 audit-fix clamp (100), garbage falls back.
    let body: Value = client
        .get(format!("{base}/api/conversations?pageSize=500"))
        .send()
        .await
        .expect("pageSize clamp")
        .json()
        .await
        .expect("body");
    assert_eq!(body["page_size"], json!(100));
    let body: Value = client
        .get(format!("{base}/api/conversations?pageSize=abc"))
        .send()
        .await
        .expect("garbage pageSize")
        .json()
        .await
        .expect("body");
    assert_eq!(body["page_size"], json!(50));

    // ── the error envelopes ────────────────────────────────────────────
    // Non-numeric savedViewId: the filter-schema 422.
    let r = client
        .get(format!("{base}/api/conversations?savedViewId=abc"))
        .send()
        .await
        .expect("bad savedViewId");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("body");
    assert_eq!(body["message"], json!("Invalid"));
    assert_eq!(body["detail"], json!(["savedViewId: Invalid"]));

    // Unknown saved view id: the reference 404 envelope.
    let r = client
        .get(format!("{base}/api/conversations?savedViewId=999"))
        .send()
        .await
        .expect("unknown view");
    assert_eq!(r.status().as_u16(), 404);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body,
        json!({"statusCode": 404, "error": "NotFound", "message": "Saved view not found."})
    );

    // aiAttribute without a value.
    let r = client
        .get(format!("{base}/api/conversations?aiAttribute=intent"))
        .send()
        .await
        .expect("missing value");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body["message"],
        json!("aiAttribute requires aiAttrValue (use 'unknown' to find tickets without a value).")
    );

    // Empty aiAttrValue fails the schema's min(1).
    let r = client
        .get(format!(
            "{base}/api/conversations?aiAttribute=intent&aiAttrValue="
        ))
        .send()
        .await
        .expect("empty value");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body["message"],
        json!("String must contain at least 1 character(s)")
    );

    // Unknown operator: the enum 422.
    let r = client
        .get(format!(
            "{base}/api/conversations?aiAttribute=intent&aiAttrValue=x&aiAttrOp=bogus"
        ))
        .send()
        .await
        .expect("bad op");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body["message"],
        json!("Invalid enum value. Expected 'equals' | 'not_equals' | 'contains' | 'not_contains' | 'gt' | 'gte' | 'lt' | 'lte', received 'bogus'")
    );

    // Vocabulary-bound comparison: the engine 422.
    let r = client
        .get(format!(
            "{base}/api/conversations?aiAttribute=urgency&aiAttrOp=gt&aiAttrValue=urgent"
        ))
        .send()
        .await
        .expect("vocab miss");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body["message"],
        json!("AI attribute filter rejected: ai_attribute 'urgency' comparison requires a value from its vocabulary (none, low, moderate, high).")
    );
}
