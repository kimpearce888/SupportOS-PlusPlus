//! IS-02 verification: the known-issues CRUD + link/unlink batch on the
//! real HTTP server (reference routes/issues.ts:28-107 + issueRepo.ts:
//! 133-242).
//!
//! - GET /api/issues/known — the reference `KnownIssueRecord` wire shape
//!   (title/symptoms/product/feature/known_cause/workaround/
//!   customer_safe_explanation/internal_explanation/status/first_seen_at/
//!   last_seen_at/conversation_count/provenance/created_at/updated_at)
//!   plus `conversation_ids` and `engineering_refs`, ordered by
//!   `last_seen_at DESC`; the port's legacy `name`/`description` columns
//!   stay off the wire (a legacy name-only row still serves its title).
//! - POST /api/issues/known — the v1.6.0 audit-fix zod schema
//!   message-for-message (Required / length bounds / closed vocabularies /
//!   positive-int array elements with dotted element paths / unknown keys
//!   stripped), then the full repo write (row + links with source +
//!   maintained counts/bounds + FTS) and the `known_issue_created` audit
//!   entry.
//! - GET /api/issues/known/:id — `{known_issue, conversations}` (4-column
//!   member rows) and the reference 404 envelope on unknown ids.
//! - PATCH — the v2.2.1 audit-fix partial schema (`{"status": 123}` is a
//!   422, not a stored "123"; `product: null` clears; `symptoms: null` is
//!   a 422), the dynamic SET + updated_at, the truthiness-gated FTS
//!   refresh, and ok:true for unknown ids (reference quirk).
//! - DELETE — FTS row + refs + links + issue in one transaction; unknown
//!   ids stay ok:true.
//! - link/unlink — INSERT OR IGNORE with the source, the maintained
//!   counts/bounds refresh, the `known_issue_linked` audit entry, the FK
//!   500 on unknown known-issue ids, and the fixed messages.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4016;

#[tokio::test]
async fn issues_known_crud_parity() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // A small mirror world: two linked conversations with known timestamps
    // (created_at carries the remote timestamp on synced-shaped rows — the
    // COALESCE adaptation), one remote_created_at row, and one legacy
    // name-only known issue (the pre-IS-02 wire wrote only `name`).
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (11, 110, 'Ada');
         INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (12, 120, 'Belle');
         INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (13, 130, 'Cleo');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at)
             VALUES (1, 101, 101, 'Export stuck at night', 'active', 1, 11, '2026-10-01 10:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at)
             VALUES (2, 102, 102, 'Export stuck again', 'closed', 1, 12, '2026-10-06 08:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at, remote_created_at)
             VALUES (3, 103, 103, 'Export stuck on mobile', 'active', 1, 13, '2026-10-02 09:00:00', '2026-10-02 08:00:00');
         INSERT INTO known_issues (id, name, status, description)
             VALUES (90, 'Legacy login bug', 'active', 'pre-IS-02 row with only name');",
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

    let audit_count = |action: &str| -> i64 {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = ?1",
            rusqlite::params![action],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    let fts_row_count = |known_issue_id: i64| -> i64 {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM fts_known_issues WHERE known_issue_id = ?1",
            rusqlite::params![known_issue_id],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };

    // ── POST — the full rich create ───────────────────────────────────
    let resp = client
        .post(format!("{base}/api/issues/known"))
        .json(&json!({
            "title": "Nightly export stuck at 90%",
            "symptoms": "Export jobs started after 23:00 stay at 90% and never finish.",
            "product": "Reports",
            "feature": "Exports",
            "known_cause": "The nightly maintenance window pauses the worker loop.",
            "workaround": "Re-run the export before 23:00.",
            "customer_safe_explanation": "A scheduled maintenance window can delay exports started late at night.",
            "internal_explanation": "ENG-9001: worker loop pause leak.",
            "status": "identified",
            "conversation_ids": [1, 2, 3],
            "provenance": "human_local",
            "hacker_field": "stripped by the schema"
        }))
        .send()
        .await
        .expect("create");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("create body");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["message"], json!("Known issue created."));
    let ki_id = body["id"].as_i64().expect("id served");
    assert_eq!(audit_count("known_issue_created"), 1);

    // ── GET list — full wire shape, ordering, joined lists ─────────────
    let resp = client
        .get(format!("{base}/api/issues/known"))
        .send()
        .await
        .expect("list");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("list body");
    let issues = body["known_issues"].as_array().expect("issues array");
    assert_eq!(issues.len(), 2, "created + legacy row");

    // last_seen_at DESC: the new issue has linked conversations (bounds
    // 2026-10-01..2026-10-06); the legacy row has NULL bounds and sorts
    // last (SQLite NULLs-last on DESC, reference engine behavior).
    let first = &issues[0];
    assert_eq!(first["id"].as_i64(), Some(ki_id));
    let expected_keys = [
        "id",
        "title",
        "symptoms",
        "product",
        "feature",
        "known_cause",
        "workaround",
        "customer_safe_explanation",
        "internal_explanation",
        "status",
        "first_seen_at",
        "last_seen_at",
        "conversation_count",
        "provenance",
        "created_at",
        "updated_at",
        "conversation_ids",
        "engineering_refs",
    ];
    let mut served: Vec<&str> = first
        .as_object()
        .expect("known issue object")
        .keys()
        .map(String::as_str)
        .collect();
    served.sort_unstable();
    let mut expected_sorted = expected_keys;
    expected_sorted.sort_unstable();
    assert_eq!(served, expected_sorted, "exact reference field set");
    assert_eq!(first["title"].as_str(), Some("Nightly export stuck at 90%"));
    assert_eq!(first["status"].as_str(), Some("identified"));
    assert_eq!(first["product"].as_str(), Some("Reports"));
    assert_eq!(first["feature"].as_str(), Some("Exports"));
    assert_eq!(
        first["symptoms"].as_str(),
        Some("Export jobs started after 23:00 stay at 90% and never finish.")
    );
    assert_eq!(
        first["known_cause"].as_str(),
        Some("The nightly maintenance window pauses the worker loop.")
    );
    assert_eq!(
        first["workaround"].as_str(),
        Some("Re-run the export before 23:00.")
    );
    assert_eq!(
        first["customer_safe_explanation"].as_str(),
        Some("A scheduled maintenance window can delay exports started late at night.")
    );
    assert_eq!(
        first["internal_explanation"].as_str(),
        Some("ENG-9001: worker loop pause leak.")
    );
    assert_eq!(first["provenance"].as_str(), Some("human_local"));
    assert_eq!(first["conversation_count"].as_i64(), Some(3));
    assert_eq!(
        first["conversation_ids"],
        json!([1, 2, 3]),
        "links in insertion order"
    );
    assert_eq!(first["engineering_refs"], json!([]));
    // The maintained bounds: MIN/MAX over the member timestamps. Conv 3
    // carries a real remote_created_at (2026-10-02 08:00:00) — the
    // COALESCE(remote_created_at, created_at) adaptation — so its bound is
    // the REMOTE value; the MIN is conv 1 (2026-10-01, the earliest) and
    // the MAX is conv 2 (2026-10-06, sync-shaped row).
    assert_eq!(first["first_seen_at"].as_str(), Some("2026-10-01 10:00:00"));
    assert_eq!(first["last_seen_at"].as_str(), Some("2026-10-06 08:00:00"));

    // The legacy name-only row still serves its title (COALESCE(title,
    // name)) with the off-wire columns absent.
    let legacy = &issues[1];
    assert_eq!(legacy["id"].as_i64(), Some(90));
    assert_eq!(legacy["title"].as_str(), Some("Legacy login bug"));
    assert_eq!(legacy["conversation_count"].as_i64(), Some(0));
    assert_eq!(legacy["conversation_ids"], json!([]));

    // The FTS row landed with the searchable fields.
    assert_eq!(fts_row_count(ki_id), 1);

    // ── POST validation — message-for-message zod parity ──────────────
    async fn probe(client: &reqwest::Client, base: &str, payload: Value) -> (u16, Value) {
        let resp = client
            .post(format!("{base}/api/issues/known"))
            .json(&payload)
            .send()
            .await
            .expect("probe");
        let status = resp.status().as_u16();
        (status, resp.json().await.expect("probe body"))
    }
    let (status, body) = probe(&client, &base, json!({})).await;
    assert_eq!(status, 422);
    assert_eq!(body["message"], json!("Invalid request (title): Required"));
    assert_eq!(
        body["issues"][0],
        json!({"path": "title", "message": "Required"})
    );

    let (status, body) = probe(&client, &base, json!({"title": 123})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (title): Expected string, received number")
    );

    let (status, body) = probe(&client, &base, json!({"title": ""})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (title): String must contain at least 1 character(s)")
    );

    let (status, body) = probe(&client, &base, json!({"title": "x".repeat(301)})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (title): String must contain at most 300 character(s)")
    );

    let (status, body) = probe(&client, &base, json!({"title": "t", "symptoms": null})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (symptoms): Expected string, received null")
    );

    let (status, body) = probe(&client, &base, json!({"title": "t", "status": "closed"})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (status): Invalid enum value. Expected 'open' | 'investigating' | 'identified' | 'monitoring' | 'resolved', received 'closed'")
    );

    let (status, body) = probe(&client, &base, json!({"title": "t", "status": 5})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (status): Expected 'open' | 'investigating' | 'identified' | 'monitoring' | 'resolved', received number")
    );

    let (status, body) = probe(
        &client,
        &base,
        json!({"title": "t", "conversation_ids": [1, "a", 2.5, 0]}),
    )
    .await;
    assert_eq!(status, 422);
    // zod collects one issue per failing element, in element order.
    assert_eq!(
        body["issues"],
        json!([
            {"path": "conversation_ids.1", "message": "Expected number, received string"},
            {"path": "conversation_ids.2", "message": "Expected integer, received float"},
            {"path": "conversation_ids.3", "message": "Number must be greater than 0"}
        ])
    );

    let (status, body) = probe(&client, &base, json!({"title": "t", "conversation_ids": 5})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (conversation_ids): Expected array, received number")
    );

    let (status, body) = probe(&client, &base, json!({"title": "t", "provenance": "god"})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (provenance): Invalid enum value. Expected 'human_local' | 'ai_generated', received 'god'")
    );

    // Unknown keys are stripped, not rejected — the create succeeds.
    let (status, body) = probe(&client, &base, json!({"title": "Bare minimum", "junk": 1})).await;
    assert_eq!(status, 200);
    let bare_id = body["id"].as_i64().expect("bare id");

    // Defaults: status 'investigating', provenance 'human_local', symptoms
    // '' (the reference's `ki.symptoms ?? ''`), no links.
    let resp = client
        .get(format!("{base}/api/issues/known/{bare_id}"))
        .send()
        .await
        .expect("bare detail");
    let body: Value = resp.json().await.expect("bare detail body");
    let ki = &body["known_issue"];
    assert_eq!(ki["status"], json!("investigating"));
    assert_eq!(ki["provenance"], json!("human_local"));
    assert_eq!(ki["symptoms"], json!(""));
    assert_eq!(ki["conversation_count"], json!(0));
    assert!(ki["first_seen_at"].is_null(), "no links -> NULL bounds");
    assert!(ki["last_seen_at"].is_null());
    assert_eq!(body["conversations"], json!([]), "IN (NULL) degradation");

    // ── GET detail — member rows and the 404 envelope ─────────────────
    let resp = client
        .get(format!("{base}/api/issues/known/{ki_id}"))
        .send()
        .await
        .expect("detail");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("detail body");
    assert_eq!(body["known_issue"]["id"], json!(ki_id));
    let conversations = body["conversations"].as_array().expect("conversations");
    assert_eq!(conversations.len(), 3);
    let conv_keys: Vec<&str> = {
        let mut ks: Vec<&str> = conversations[0]
            .as_object()
            .expect("conv object")
            .keys()
            .map(String::as_str)
            .collect();
        ks.sort_unstable();
        ks
    };
    assert_eq!(
        conv_keys,
        vec!["id", "number", "status", "subject"],
        "4-column member rows (no timestamps)"
    );

    let resp = client
        .get(format!("{base}/api/issues/known/9999"))
        .send()
        .await
        .expect("unknown detail");
    assert_eq!(resp.status().as_u16(), 404);
    let body: Value = resp.json().await.expect("404 body");
    assert_eq!(
        body,
        json!({"statusCode": 404, "error": "NotFound", "message": "Known issue not found."})
    );

    // ── PATCH — the v2.2.1 audit-fix semantics ─────────────────────────
    let resp = client
        .patch(format!("{base}/api/issues/known/{ki_id}"))
        .json(&json!({"status": "monitoring", "workaround": "Re-run before 23:00 or after 06:00."}))
        .send()
        .await
        .expect("patch");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("patch body");
    assert_eq!(body, json!({"ok": true, "message": "Known issue updated."}));
    assert_eq!(audit_count("known_issue_updated"), 1);

    let resp = client
        .get(format!("{base}/api/issues/known/{ki_id}"))
        .send()
        .await
        .expect("re-read");
    let body: Value = resp.json().await.expect("re-read body");
    assert_eq!(body["known_issue"]["status"], json!("monitoring"));
    assert_eq!(
        body["known_issue"]["workaround"],
        json!("Re-run before 23:00 or after 06:00.")
    );

    // `product: null` clears the column (nullable field), `symptoms: null`
    // is a 422 (not nullable), and `{"status": 123}` is a 422 — the loose
    // cast the v2.2.1 audit fixed stored "123".
    let resp = client
        .patch(format!("{base}/api/issues/known/{ki_id}"))
        .json(&json!({"product": null}))
        .send()
        .await
        .expect("patch null product");
    assert_eq!(resp.status().as_u16(), 200);
    let resp = client
        .get(format!("{base}/api/issues/known/{ki_id}"))
        .send()
        .await
        .expect("re-read 2");
    let body: Value = resp.json().await.expect("re-read 2 body");
    assert!(body["known_issue"]["product"].is_null());

    let resp = client
        .patch(format!("{base}/api/issues/known/{ki_id}"))
        .json(&json!({"symptoms": null}))
        .send()
        .await
        .expect("patch null symptoms");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("422 body");
    assert_eq!(
        body["message"],
        json!("Invalid request (symptoms): Expected string, received null")
    );

    let resp = client
        .patch(format!("{base}/api/issues/known/{ki_id}"))
        .json(&json!({"status": 123}))
        .send()
        .await
        .expect("patch numeric status");
    assert_eq!(resp.status().as_u16(), 422);

    // Empty patch: ok:true, nothing touched, audit still written.
    let resp = client
        .patch(format!("{base}/api/issues/known/{ki_id}"))
        .json(&json!({}))
        .send()
        .await
        .expect("empty patch");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(audit_count("known_issue_updated"), 3);

    // Unknown id: zero rows updated, still ok:true (reference quirk).
    let resp = client
        .patch(format!("{base}/api/issues/known/9999"))
        .json(&json!({"status": "resolved"}))
        .send()
        .await
        .expect("patch unknown");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(audit_count("known_issue_updated"), 4);

    // The truthiness-gated FTS refresh: a title change re-indexes the row.
    let resp = client
        .patch(format!("{base}/api/issues/known/{ki_id}"))
        .json(&json!({"title": "Nightly export pause (renamed)"}))
        .send()
        .await
        .expect("patch title");
    assert_eq!(resp.status().as_u16(), 200);
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let fts_title: String = conn
            .query_row(
                "SELECT title FROM fts_known_issues WHERE known_issue_id = ?1",
                rusqlite::params![ki_id],
                |r| r.get(0),
            )
            .expect("fts row");
        assert_eq!(fts_title, "Nightly export pause (renamed)");
    }

    // ── link/unlink — source, maintained counts, audit, FK ───────────
    // The created issue has 3 links; add one more and the counts/bounds
    // follow.
    let resp = client
        .post(format!("{base}/api/issues/known/{ki_id}/link/1"))
        .send()
        .await
        .expect("duplicate link");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("duplicate link body");
    assert_eq!(
        body,
        json!({"ok": true, "message": "Conversation linked to known issue."})
    );
    let resp = client
        .get(format!("{base}/api/issues/known/{ki_id}"))
        .send()
        .await
        .expect("re-read 3");
    let body: Value = resp.json().await.expect("re-read 3 body");
    assert_eq!(
        body["known_issue"]["conversation_count"],
        json!(3),
        "INSERT OR IGNORE keeps the duplicate link out"
    );
    assert_eq!(body["known_issue"]["conversation_ids"], json!([1, 2, 3]));

    // Unlink one member: counts drop, bounds recompute.
    let resp = client
        .delete(format!("{base}/api/issues/known/{ki_id}/link/2"))
        .send()
        .await
        .expect("unlink");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("unlink body");
    assert_eq!(
        body,
        json!({"ok": true, "message": "Conversation unlinked."})
    );
    let resp = client
        .get(format!("{base}/api/issues/known/{ki_id}"))
        .send()
        .await
        .expect("re-read 4");
    let body: Value = resp.json().await.expect("re-read 4 body");
    assert_eq!(body["known_issue"]["conversation_count"], json!(2));
    assert_eq!(
        body["known_issue"]["first_seen_at"],
        json!("2026-10-01 10:00:00"),
        "conv 1 (2026-10-01) is now the earliest member"
    );
    assert_eq!(
        body["known_issue"]["last_seen_at"],
        json!("2026-10-02 08:00:00"),
        "conv 3's REMOTE value (not its 09:00 created_at) — the COALESCE adaptation"
    );

    // The link audit entry carries the conversation id.
    assert!(audit_count("known_issue_linked") >= 1);
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let (actor, conv_col): (String, Option<i64>) = conn
            .query_row(
                "SELECT actor, conversation_id FROM audit_log WHERE action = 'known_issue_linked' LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("link audit row");
        assert_eq!(actor, "user");
        assert_eq!(conv_col, Some(1));
    }

    // Linking an unknown known issue violates the link table's FK
    // (foreign_keys=ON) — the 500 envelope, like the reference's FK
    // failure.
    let resp = client
        .post(format!("{base}/api/issues/known/4242/link/1"))
        .send()
        .await
        .expect("link unknown issue");
    assert_eq!(resp.status().as_u16(), 500);
    let body: Value = resp.json().await.expect("500 body");
    assert_eq!(body["statusCode"], json!(500));
    assert_eq!(body["error"], json!("InternalError"));

    // ── DELETE — cascades, fixed message, unknown ids stay ok ──────────
    // Give the issue a reference row to cascade away with it.
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO known_issue_refs (known_issue_id, system, reference_id, title, status)
             VALUES (?1, 'linear', 'ENG-9001', 'Worker pause leak', 'in progress')",
            rusqlite::params![ki_id],
        )
        .expect("ref row");
    }
    let resp = client
        .delete(format!("{base}/api/issues/known/{ki_id}"))
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("delete body");
    assert_eq!(body, json!({"ok": true, "message": "Known issue deleted."}));
    assert_eq!(fts_row_count(ki_id), 0, "FTS row removed");
    {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let (issue_rows, ref_rows, link_rows): (i64, i64, i64) = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM known_issues WHERE id = ?1),
                    (SELECT COUNT(*) FROM known_issue_refs WHERE known_issue_id = ?1),
                    (SELECT COUNT(*) FROM known_issue_links WHERE known_issue_id = ?1)",
                rusqlite::params![ki_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((issue_rows, ref_rows, link_rows), (0, 0, 0));
    }

    let resp = client
        .delete(format!("{base}/api/issues/known/9999"))
        .send()
        .await
        .expect("delete unknown");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("delete unknown body");
    assert_eq!(body, json!({"ok": true, "message": "Known issue deleted."}));

    // The remaining rows are the legacy row and the bare-minimum create
    // from the validation section; both still serve their titles (the
    // legacy name-only row via COALESCE(title, name)).
    let resp = client
        .get(format!("{base}/api/issues/known"))
        .send()
        .await
        .expect("final list");
    let body: Value = resp.json().await.expect("final list body");
    let issues = body["known_issues"].as_array().expect("final issues");
    assert_eq!(issues.len(), 2);
    let titles: Vec<&str> = issues
        .iter()
        .map(|i| i["title"].as_str().expect("title"))
        .collect();
    assert!(titles.contains(&"Legacy login bug"));
    assert!(titles.contains(&"Bare minimum"));
}
