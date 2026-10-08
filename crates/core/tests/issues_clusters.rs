//! IS-01 verification: the issue-cluster serving batch on the real HTTP
//! server (reference routes/issues.ts:6-26 + issueRepo.ts:88-131).
//!
//! - GET /api/issues/clusters — the reference `IssueCluster` wire shape:
//!   id/title/summary/category/product/feature/conversation_count/
//!   customer_count/first_seen_at/last_seen_at/trend/known_issue_id/
//!   ai_generated/created_at/updated_at/provenance + conversation_ids,
//!   ordered by conversation_count DESC; the port's legacy `name`/`status`
//!   columns stay off the wire.
//! - GET /api/issues/clusters/:id — `{cluster, conversations}` where the
//!   conversations are (id, number, subject, status, remote_created_at)
//!   member rows (COALESCE(remote_created_at, created_at) adaptation), and
//!   the reference 404 envelope on unknown ids.
//! - DELETE /api/issues/clusters/:id — `{ok, message}`; the members row
//!   cascades away and the conversations are untouched; unknown ids stay
//!   ok:true exactly like the reference.
//!
//! Clusters are seeded through the same public `upsert_cluster`
//! (reference issues.upsertCluster) the AI clustering pipeline and the
//! demo seed use, so the counts under test are the maintained ones.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4013;

fn upsert(conn: &rusqlite::Connection, c: spp_core::ai_pipeline::ClusterUpsert) -> i64 {
    spp_core::ai_pipeline::upsert_cluster(conn, &c).expect("upsert_cluster")
}

#[tokio::test]
async fn issues_clusters_serving_parity() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // A small mirror world. c1/c3 carry only created_at (sync-shaped rows);
    // c2 carries a real remote_created_at; c4 is the billing cluster's only
    // member.
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at)
             VALUES (1, 101, 101, 'Cannot log in since morning', 'active',  1, 11, '2026-10-01 10:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at, remote_created_at)
             VALUES (2, 102, 102, 'Login page 500', 'closed', 1, 11, '2026-10-03 09:00:00', '2026-10-02 09:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at)
             VALUES (3, 103, 103, 'Password reset loop', 'active',  1, 12, '2026-10-05 11:00:00');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at)
             VALUES (4, 104, 104, 'Invoice VAT wrong', 'closed', 1, 13, '2026-10-06 08:00:00');",
    )
    .expect("seed world");

    // The cluster the reference's demo seed would recognize: three login
    // conversations from two customers, linked to a known issue.
    let login = upsert(
        &conn,
        spp_core::ai_pipeline::ClusterUpsert {
            title: "login outage".to_string(),
            summary: "Customers cannot sign in; the login page intermittently 500s.".to_string(),
            category: Some("Authentication".to_string()),
            product: Some("Access".to_string()),
            feature: Some("Login".to_string()),
            known_issue_id: Some(7),
            ai_generated: true,
            conversation_ids: vec![1, 2, 3],
        },
    );
    let billing = upsert(
        &conn,
        spp_core::ai_pipeline::ClusterUpsert {
            title: "billing wave".to_string(),
            summary: "Failed charges and invoice/VAT questions from finance contacts.".to_string(),
            category: Some("Billing".to_string()),
            product: Some("Billing".to_string()),
            feature: Some("Payments".to_string()),
            known_issue_id: None,
            ai_generated: false,
            conversation_ids: vec![4],
        },
    );
    // Pin trends away from compute_trends' clock math so the served value is
    // deterministic (compute_trends is a separate maintenance pass).
    conn.execute(
        "UPDATE issue_clusters SET trend = 'rising' WHERE id = ?1",
        rusqlite::params![login],
    )
    .unwrap();
    conn.execute(
        "UPDATE issue_clusters SET trend = 'stable' WHERE id = ?1",
        rusqlite::params![billing],
    )
    .unwrap();

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

    // ── GET /api/issues/clusters — full wire shape, count order ───────
    let resp = client
        .get(format!("{base}/api/issues/clusters"))
        .send()
        .await
        .expect("list");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("list body");
    let clusters = body["clusters"].as_array().expect("clusters array");
    assert_eq!(clusters.len(), 2, "both seeded clusters served");

    let first = &clusters[0];
    assert_eq!(
        first["id"].as_i64(),
        Some(login),
        "conversation_count DESC puts the 3-member cluster first"
    );
    // Exactly the reference IssueCluster field set + conversation_ids — the
    // port's legacy name/status columns must stay off the wire.
    let expected_keys = [
        "id",
        "title",
        "summary",
        "category",
        "product",
        "feature",
        "conversation_count",
        "customer_count",
        "first_seen_at",
        "last_seen_at",
        "trend",
        "known_issue_id",
        "ai_generated",
        "created_at",
        "updated_at",
        "provenance",
        "conversation_ids",
    ];
    let mut served: Vec<&str> = first
        .as_object()
        .expect("cluster object")
        .keys()
        .map(String::as_str)
        .collect();
    served.sort_unstable();
    let mut expected_sorted = expected_keys;
    expected_sorted.sort_unstable();
    assert_eq!(served, expected_sorted, "exact reference field set");
    assert_eq!(first["title"].as_str(), Some("login outage"));
    assert!(first["summary"]
        .as_str()
        .expect("summary")
        .starts_with("Customers cannot sign in"));
    assert_eq!(first["category"].as_str(), Some("Authentication"));
    assert_eq!(first["product"].as_str(), Some("Access"));
    assert_eq!(first["feature"].as_str(), Some("Login"));
    assert_eq!(first["conversation_count"].as_i64(), Some(3));
    assert_eq!(
        first["customer_count"].as_i64(),
        Some(2),
        "DISTINCT customers across members (11 twice + 12 once)"
    );
    assert_eq!(first["trend"].as_str(), Some("rising"));
    assert_eq!(first["known_issue_id"].as_i64(), Some(7));
    assert_eq!(first["ai_generated"].as_i64(), Some(1));
    assert_eq!(
        first["first_seen_at"].as_str(),
        Some("2026-10-01 10:00:00"),
        "MIN(COALESCE(remote_created_at, created_at))"
    );
    assert_eq!(
        first["last_seen_at"].as_str(),
        Some("2026-10-05 11:00:00"),
        "MAX(COALESCE(remote_created_at, created_at))"
    );
    assert_eq!(first["conversation_ids"], json!([1, 2, 3]));

    let second = &clusters[1];
    assert_eq!(second["id"].as_i64(), Some(billing));
    assert_eq!(second["title"].as_str(), Some("billing wave"));
    assert_eq!(second["conversation_count"].as_i64(), Some(1));
    assert_eq!(second["customer_count"].as_i64(), Some(1));
    assert_eq!(
        second["ai_generated"].as_i64(),
        Some(0),
        "ai_generated: false stores 0"
    );
    assert_eq!(second["known_issue_id"], Value::Null);
    assert_eq!(second["conversation_ids"], json!([4]));

    // ── GET /api/issues/clusters/:id — cluster + conversations ─────────
    let resp = client
        .get(format!("{base}/api/issues/clusters/{login}"))
        .send()
        .await
        .expect("detail");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("detail body");
    let cluster = &body["cluster"];
    assert_eq!(cluster["title"].as_str(), Some("login outage"));
    assert_eq!(cluster["conversation_ids"], json!([1, 2, 3]));
    let conversations = body["conversations"].as_array().expect("conversations");
    assert_eq!(conversations.len(), 3);
    let by_id = |id: i64| {
        conversations
            .iter()
            .find(|c| c["id"].as_i64() == Some(id))
            .unwrap_or_else(|| panic!("conversation {id} missing"))
            .clone()
    };
    for c in conversations {
        let mut keys: Vec<&str> = c
            .as_object()
            .expect("conversation object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let mut want = ["id", "number", "remote_created_at", "status", "subject"];
        want.sort_unstable();
        assert_eq!(keys, want, "exact reference conversation field set");
    }
    assert_eq!(
        by_id(1)["remote_created_at"].as_str(),
        Some("2026-10-01 10:00:00"),
        "created_at served when remote_created_at is NULL"
    );
    assert_eq!(
        by_id(2)["remote_created_at"].as_str(),
        Some("2026-10-02 09:00:00"),
        "remote_created_at wins when set"
    );
    assert_eq!(
        by_id(3)["remote_created_at"].as_str(),
        Some("2026-10-05 11:00:00")
    );
    assert_eq!(by_id(1)["number"].as_i64(), Some(101));
    assert_eq!(by_id(2)["subject"].as_str(), Some("Login page 500"));
    assert_eq!(by_id(3)["status"].as_str(), Some("active"));

    // ── GET /api/issues/clusters/:id — the reference 404 envelope ──────
    let resp = client
        .get(format!("{base}/api/issues/clusters/99999"))
        .send()
        .await
        .expect("unknown detail");
    assert_eq!(resp.status().as_u16(), 404);
    let body: Value = resp.json().await.expect("404 body");
    assert_eq!(
        body,
        json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": "Cluster not found."
        })
    );

    // ── DELETE /api/issues/clusters/:id — reference ok shape ───────────
    let resp = client
        .delete(format!("{base}/api/issues/clusters/{billing}"))
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("delete body");
    assert_eq!(
        body,
        json!({
            "ok": true,
            "message": "Cluster deleted (conversations are untouched)."
        })
    );

    // The list no longer serves it; the members row cascaded away; the
    // conversation itself is untouched.
    let resp = client
        .get(format!("{base}/api/issues/clusters"))
        .send()
        .await
        .expect("list after delete");
    let body: Value = resp.json().await.expect("list after delete body");
    let clusters = body["clusters"].as_array().expect("clusters array");
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0]["id"].as_i64(), Some(login));
    let resp = client
        .get(format!("{base}/api/issues/clusters/{billing}"))
        .send()
        .await
        .expect("detail after delete");
    assert_eq!(resp.status().as_u16(), 404);
    {
        let guard = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let members: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM issue_cluster_conversations WHERE cluster_id = ?1",
                rusqlite::params![billing],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(members, 0, "members cascade with the cluster row");
        let convs: i64 = guard
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(convs, 4, "conversations are untouched by the delete");
    }

    // ── DELETE unknown id — still ok:true (reference behavior) ─────────
    let resp = client
        .delete(format!("{base}/api/issues/clusters/424242"))
        .send()
        .await
        .expect("delete unknown");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("delete unknown body");
    assert_eq!(body["ok"].as_bool(), Some(true));

    // ── upsert_cluster is title-keyed (reference upsertCluster) ────────
    {
        let guard = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let again = upsert(
            &guard,
            spp_core::ai_pipeline::ClusterUpsert {
                title: "login outage".to_string(),
                summary: "UPDATED summary".to_string(),
                category: None,
                product: None,
                feature: None,
                known_issue_id: None,
                ai_generated: true,
                conversation_ids: vec![1, 2],
            },
        );
        assert_eq!(again, login, "same title upserts onto the same row");
        let (summary, ki, count): (String, Option<i64>, i64) = guard
            .query_row(
                "SELECT summary, known_issue_id, conversation_count FROM issue_clusters WHERE id = ?1",
                rusqlite::params![login],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(summary, "UPDATED summary");
        assert_eq!(ki, None, "update path writes known_issue_id");
        assert_eq!(count, 3, "members are never removed on re-upsert");
        let rows: i64 = guard
            .query_row("SELECT COUNT(*) FROM issue_clusters", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "no duplicate cluster on re-upsert");
    }
}
