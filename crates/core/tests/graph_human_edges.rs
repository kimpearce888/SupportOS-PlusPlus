//! GR-02 verification: the human-edge half of the support graph adopts the
//! reference wire contract on the REAL HTTP server.
//!
//! Reference: supportos @ c346fb51 — src/server/routes/graph.ts +
//! src/server/graph/graphService.ts + migration 016 (support_graph_edges,
//! renamed graph_edges in the port).
//!
//! - GET /api/graph/meta — the 12-kind union, the closed 5-relation
//!   human vocabulary (related_to, depends_on, blocks, mentions,
//!   duplicate_of) and the reference's notes array.
//! - POST /api/graph/edges — the Zod-shaped body contract (422 envelope
//!   with `Invalid request (path): message` + issues array, message-for-
//!   message identical to zod 3.24.2), then the endpoint checks in the
//!   reference's fixed order: self edge → 409, source 404, target 404,
//!   duplicate → 409; success answers {ok, edge} with BOTH endpoints
//!   resolved as GraphNodeRefs.
//! - GET /api/graph/edges — {edges, total} with JS-falsy limit/offset
//!   semantics and newest-first ordering.
//! - DELETE /api/graph/edges/:id — 422 on non-positive ids, 404 on
//!   unknown ids, {ok} only on a real removal.
//! - neighbors/subgraph (interim envelopes) + stats' human_edges count
//!   read the same human-edge store.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4012;

#[tokio::test]
async fn graph_human_edges_parity() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // A small mirror world: two customers, a conversation, a known issue,
    // an incident and an agent (the kinds the probes below link).
    conn.execute_batch(
        "INSERT INTO organizations (id, remote_id, name, domains) VALUES (5, 55, 'Acme', 'acme.io');
         INSERT INTO customers (id, remote_id, first_name, last_name) VALUES (9, 99, 'Ada', 'Lovelace');
         UPDATE customers SET organization_id = 5 WHERE id = 9;
         INSERT INTO customers (id, remote_id, first_name, last_name) VALUES (10, 100, 'Carol', 'Client');
         INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id)
             VALUES (3, 33, 33, 'Refund please', 'active', 1, 9);
         INSERT INTO known_issues (id, name, status) VALUES (7, 'Login bug', 'active');
         INSERT INTO incidents (id, code, title, status, severity, source)
             VALUES (2, 'INC-1', 'Outage', 'resolved', 'sev2', 'manual');
         INSERT INTO users (id, remote_id, first_name, last_name, email)
             VALUES (8, 88, 'Grace', 'Hopper', 'grace@example.com');",
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

    // ── GET /api/graph/meta — the 5-relation vocabulary ───────────────
    let resp = client
        .get(format!("{base}/api/graph/meta"))
        .send()
        .await
        .expect("meta");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("meta body");
    assert_eq!(
        body["human_relations"],
        json!([
            "related_to",
            "depends_on",
            "blocks",
            "mentions",
            "duplicate_of"
        ])
    );
    assert_eq!(body["node_kinds"].as_array().map(Vec::len), Some(12));
    assert_eq!(
        body["notes"],
        json!([
            "Derived edges are computed live from the local mirror; only human-asserted edges are stored.",
            "Connector rows have no derived links by design."
        ])
    );

    // ── Empty store lists nothing ──────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/graph/edges"))
        .send()
        .await
        .expect("list empty");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("list empty body");
    assert_eq!(body["total"], 0);
    assert_eq!(body["edges"].as_array().map(Vec::len), Some(0));

    // ── POST validation: the reference Zod envelope ─────────────────────
    let post = |body: Value| {
        let client = client.clone();
        let url = format!("{base}/api/graph/edges");
        async move {
            let r = client.post(&url).json(&body).send().await.expect("post");
            let status = r.status().as_u16();
            let b: Value = r.json().await.expect("post body");
            (status, b)
        }
    };

    // Empty body: every required field, in schema order, with the issues array.
    let (status, body) = post(json!({})).await;
    assert_eq!(status, 422);
    assert_eq!(body["error"], "ValidationError");
    assert_eq!(body["statusCode"], 422);
    assert_eq!(body["message"], "Invalid request (source_kind): Required");
    assert_eq!(
        body["issues"],
        json!([
            {"path": "source_kind", "message": "Required"},
            {"path": "source_local_id", "message": "Required"},
            {"path": "target_kind", "message": "Required"},
            {"path": "target_local_id", "message": "Required"},
            {"path": "relation", "message": "Required"}
        ])
    );

    // Wrong enum string — message-for-message zod parity.
    let (status, body) = post(json!({
        "source_kind": "robot", "source_local_id": 1,
        "target_kind": "customer", "target_local_id": 2,
        "relation": "related"
    }))
    .await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (source_kind): Invalid enum value. Expected 'customer' | 'organization' | 'conversation' | 'known_issue' | 'issue_cluster' | 'incident' | 'knowledge_document' | 'agent' | 'campaign' | 'product' | 'custom_object' | 'connector_data', received 'robot'")
    );
    assert_eq!(
        body["issues"][1]["message"],
        json!("Invalid enum value. Expected 'related_to' | 'depends_on' | 'blocks' | 'mentions' | 'duplicate_of', received 'related'")
    );

    // Non-string enum: zod's invalid_type rendering.
    let (status, body) = post(json!({
        "source_kind": 5, "source_local_id": 1,
        "target_kind": "customer", "target_local_id": 2,
        "relation": "related_to"
    }))
    .await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        json!("Invalid request (source_kind): Expected 'customer' | 'organization' | 'conversation' | 'known_issue' | 'issue_cluster' | 'incident' | 'knowledge_document' | 'agent' | 'campaign' | 'product' | 'custom_object' | 'connector_data', received number")
    );

    // Number-field checks: string / float / zero.
    let valid = json!({
        "source_kind": "customer", "source_local_id": 9,
        "target_kind": "conversation", "target_local_id": 3,
        "relation": "related_to"
    });
    let mut bad = valid.clone();
    bad["source_local_id"] = json!("9");
    let (status, body) = post(bad).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        "Invalid request (source_local_id): Expected number, received string"
    );
    let mut bad = valid.clone();
    bad["target_local_id"] = json!(3.5);
    let (status, body) = post(bad).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        "Invalid request (target_local_id): Expected integer, received float"
    );
    let mut bad = valid.clone();
    bad["source_local_id"] = json!(0);
    let (status, body) = post(bad).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        "Invalid request (source_local_id): Number must be greater than or equal to 1"
    );

    // note caps at 500 exactly like z.string().max(500).
    let mut bad = valid.clone();
    bad["note"] = json!("x".repeat(501));
    let (status, body) = post(bad).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["message"],
        "Invalid request (note): String must contain at most 500 character(s)"
    );

    // ── Endpoint checks, in the reference's fixed order ─────────────────
    // Self edge first (409).
    let (status, body) = post(json!({
        "source_kind": "customer", "source_local_id": 9,
        "target_kind": "customer", "target_local_id": 9,
        "relation": "duplicate_of"
    }))
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["error"], "Conflict");
    assert_eq!(body["message"], "A node cannot be linked to itself.");

    // Unknown source (404), checked before the target.
    let (status, body) = post(json!({
        "source_kind": "customer", "source_local_id": 999,
        "target_kind": "customer", "target_local_id": 888,
        "relation": "related_to"
    }))
    .await;
    assert_eq!(status, 404);
    assert_eq!(body["error"], "NotFound");
    assert_eq!(body["message"], "Source node not found.");

    // Unknown target (404).
    let (status, body) = post(json!({
        "source_kind": "customer", "source_local_id": 9,
        "target_kind": "conversation", "target_local_id": 999,
        "relation": "related_to"
    }))
    .await;
    assert_eq!(status, 404);
    assert_eq!(body["message"], "Target node not found.");

    // ── A good link round-trips with resolved endpoints ─────────────────
    let (status, body) = post(json!({
        "source_kind": "customer", "source_local_id": 9,
        "target_kind": "conversation", "target_local_id": 3,
        "relation": "related_to", "note": "escalation context"
    }))
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], true);
    let edge = &body["edge"];
    let edge_id = edge["id"].as_i64().expect("edge id");
    assert_eq!(edge["relation"], "related_to");
    assert_eq!(edge["note"], "escalation context");
    assert_eq!(edge["created_by"], Value::Null);
    assert_eq!(edge["source"]["kind"], "customer");
    assert_eq!(edge["source"]["local_id"], 9);
    assert_eq!(edge["source"]["label"], "Ada Lovelace");
    assert_eq!(edge["source"]["sublabel"], "Acme");
    assert_eq!(edge["source"]["deleted"], false);
    assert_eq!(edge["target"]["kind"], "conversation");
    assert_eq!(edge["target"]["label"], "#33 Refund please");
    assert!(edge["created_at"].as_str().is_some_and(|s| !s.is_empty()));

    // The exact 5-tuple is a duplicate (409).
    let (status, body) = post(json!({
        "source_kind": "customer", "source_local_id": 9,
        "target_kind": "conversation", "target_local_id": 3,
        "relation": "related_to"
    }))
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["message"], "This edge already exists (duplicate).");

    // Two more links with a DIFFERENT relation + another source: listing
    // orders newest-first and paginates.
    let (status, _) = post(json!({
        "source_kind": "customer", "source_local_id": 9,
        "target_kind": "conversation", "target_local_id": 3,
        "relation": "depends_on"
    }))
    .await;
    assert_eq!(status, 200);
    let (status, _) = post(json!({
        "source_kind": "known_issue", "source_local_id": 7,
        "target_kind": "incident", "target_local_id": 2,
        "relation": "duplicate_of"
    }))
    .await;
    assert_eq!(status, 200);

    let resp = client
        .get(format!("{base}/api/graph/edges"))
        .send()
        .await
        .expect("list");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("list body");
    assert_eq!(body["total"], 3);
    let edges = body["edges"].as_array().expect("edges");
    assert_eq!(edges.len(), 3);
    assert_eq!(edges[0]["relation"], "duplicate_of", "newest first");
    assert_eq!(edges[0]["source"]["label"], "Login bug");
    assert_eq!(edges[0]["target"]["label"], "INC-1 Outage");

    // JS-falsy limit/offset: limit=0 falls back to 50, not 1; offset
    // pages the newest-first order.
    let resp = client
        .get(format!("{base}/api/graph/edges?limit=0"))
        .send()
        .await
        .expect("list limit=0");
    let body: Value = resp.json().await.expect("body");
    assert_eq!(
        body["edges"].as_array().map(Vec::len),
        Some(3),
        "limit=0 -> default 50"
    );
    let resp = client
        .get(format!("{base}/api/graph/edges?limit=1&offset=1"))
        .send()
        .await
        .expect("list paged");
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["total"], 3);
    assert_eq!(body["edges"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["edges"][0]["relation"], "depends_on");

    // ── neighbors / subgraph read the same store (reference envelopes) ───
    let resp = client
        .get(format!("{base}/api/graph/neighbors/customer/9"))
        .send()
        .await
        .expect("neighbors");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("neighbors body");
    assert_eq!(body["node"]["kind"], "customer");
    // Four edges touch customer 9: the two stored human edges (related_to +
    // depends_on to the same conversation — the reference serves EDGES, not
    // unique nodes) PLUS the derived belongs_to (organization) and involves
    // (the conversation) edges from the GR-01 layer.
    assert_eq!(body["total_edges"], 4);
    let edges = body["edges"].as_array().expect("edges array");
    assert_eq!(edges.len(), 4);
    // Sorted by (relation, target label): belongs_to < depends_on <
    // involves < related_to.
    assert_eq!(edges[0]["relation"], "belongs_to");
    assert_eq!(edges[0]["origin"], "helpscout_mirror");
    assert_eq!(edges[0]["target"]["kind"], "organization");
    assert_eq!(edges[0]["target"]["label"], "Acme");
    assert_eq!(edges[1]["relation"], "depends_on");
    assert_eq!(edges[1]["target"]["kind"], "conversation");
    assert_eq!(edges[1]["target"]["label"], "#33 Refund please");
    assert_eq!(edges[1]["origin"], "human_local");
    assert_eq!(edges[2]["relation"], "involves");
    assert_eq!(edges[2]["origin"], "helpscout_mirror");
    assert_eq!(edges[2]["source"]["kind"], "conversation");
    assert_eq!(edges[2]["target"]["kind"], "customer");
    assert_eq!(edges[3]["relation"], "related_to");
    assert_eq!(edges[3]["source"]["kind"], "customer");

    // The target side serves the incoming edge too.
    let resp = client
        .get(format!("{base}/api/graph/neighbors/incident/2"))
        .send()
        .await
        .expect("neighbors incoming");
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["total_edges"], 1);
    let edges = body["edges"].as_array().expect("edges array");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["source"]["kind"], "known_issue");
    assert_eq!(edges[0]["target"]["kind"], "incident");

    let resp = client
        .get(format!("{base}/api/graph/subgraph/customer/9"))
        .send()
        .await
        .expect("subgraph");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("subgraph body");
    assert_eq!(body["seeds"][0]["label"], "Ada Lovelace", "seed first");
    let nodes = body["nodes"].as_array().expect("nodes array");
    assert_eq!(nodes[0]["label"], "Ada Lovelace", "center first");
    let edges = body["edges"].as_array().expect("edges array");
    // Two human edges + the derived belongs_to and involves.
    assert_eq!(edges.len(), 4, "human + derived edges");
    assert_eq!(
        edges
            .iter()
            .filter(|e| e["origin"] == "human_local")
            .count(),
        2,
        "both outgoing human edges"
    );
    assert_eq!(edges[0]["origin"], "helpscout_mirror");

    // stats counts the human-edge store (the reference per-kind shape).
    let resp = client
        .get(format!("{base}/api/graph/stats"))
        .send()
        .await
        .expect("stats");
    let body: Value = resp.json().await.expect("stats body");
    assert_eq!(body["human_edges"], 3);
    let human_edge_row = body["edges"]
        .as_array()
        .expect("edge rows")
        .iter()
        .find(|e| e["relation"] == "human_edge")
        .expect("human_edge stat");
    assert_eq!(human_edge_row["count"], 3);
    assert_eq!(human_edge_row["origin"], "human_local");

    // ── DELETE: 422 / 404 / ok, in the reference's words ───────────────
    let resp = client
        .delete(format!("{base}/api/graph/edges/0"))
        .send()
        .await
        .expect("delete 0");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["message"], "Edge id must be a positive integer.");

    let resp = client
        .delete(format!("{base}/api/graph/edges/999"))
        .send()
        .await
        .expect("delete unknown");
    assert_eq!(resp.status().as_u16(), 404);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["error"], "NotFound");
    assert_eq!(body["message"], "Human edge not found.");

    let resp = client
        .delete(format!("{base}/api/graph/edges/{edge_id}"))
        .send()
        .await
        .expect("delete ok");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["ok"], true);

    // A second removal of the same id is a 404; the store shrinks.
    let resp = client
        .delete(format!("{base}/api/graph/edges/{edge_id}"))
        .send()
        .await
        .expect("delete again");
    assert_eq!(resp.status().as_u16(), 404);
    let resp = client
        .get(format!("{base}/api/graph/edges"))
        .send()
        .await
        .expect("list after delete");
    let body: Value = resp.json().await.expect("body");
    assert_eq!(body["total"], 2);

    // The neighbors view reflects the removal: the depends_on edge remains,
    // alongside the derived belongs_to + involves edges.
    let resp = client
        .get(format!("{base}/api/graph/neighbors/customer/9"))
        .send()
        .await
        .expect("neighbors after delete");
    let body: Value = resp.json().await.expect("body");
    let edges = body["edges"].as_array().expect("edges array");
    assert_eq!(
        edges
            .iter()
            .filter(|e| e["relation"] == "depends_on")
            .count(),
        1,
        "the depends_on edge remains"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|e| e["relation"] == "related_to")
            .count(),
        0,
        "the removed edge is gone"
    );
    assert_eq!(edges.len(), 3);
    assert_eq!(body["total_edges"], 3);
}
