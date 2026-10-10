//! GR-03 verification: the graph read layer on the real HTTP server
//! (reference graph.ts + graphService.ts stats/search/node/neighbors/
//! subgraph).
//!
//! - GET /api/graph/stats — live per-kind node counts, per-relation edge
//!   counts with origins (18 rows), the human-edge total and the reference
//!   notes; every count is computed from the mirror tables.
//! - GET /api/graph/search — LIKE-escaped bounded search (per-kind cap 10,
//!   80 total) over the node labels, the exact-number conversation match,
//!   the kinds filter, and the reference 422s (query too long / unknown
//!   kind).
//! - GET /api/graph/node/:kind/:id — `{node, edge_count}` with the
//!   positive-integer/kind 422s and the 404 envelope.
//! - GET /api/graph/neighbors/:kind/:id — the `{node, edges,
//!   total_edges, truncated, notes}` envelope over the human-edge store
//!   both directions, the direction/limit params (1..200) and their 422s.
//! - GET /api/graph/subgraph/:kind/:id — the bounded BFS (depth 1..2,
//!   120-node cap, 40 edges per frontier node) serving `{seeds, nodes,
//!   edges, truncated, depth_reached, notes}`; depth 3 is a 422.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4021;

#[tokio::test]
async fn graph_read_layer_parity() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // A small mirror world: org with two customers, one assigned
    // conversation, a known issue, an incident, an agent, a campaign, a
    // product, a knowledge doc — plus three human edges between customer
    // 11, conversation 1 and known issue 1.
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO organizations (id, remote_id, name, domains) VALUES (1, 21, 'Acme', '[\"acme.com\"]');
         INSERT INTO users (id, remote_id, first_name, last_name, email) VALUES (7, 70, 'Dana', 'Reyes', 'dana@example.com');
         INSERT INTO customers (id, remote_id, first_name, last_name, organization_id) VALUES (11, 110, 'Ada', 'Lovelace', 1);
         INSERT INTO customers (id, remote_id, first_name, last_name, organization_id) VALUES (12, 120, 'Belle', 'Node', 1);
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id)
             VALUES (1, 101, 101, 'Export stuck at night', 'active', 1, 11, 7);
         INSERT INTO known_issues (id, name, status, description, title)
             VALUES (1, 'Login loop', 'investigating', 'legacy name row', 'Login loop after password reset');
         INSERT INTO incidents (id, code, title, status) VALUES (1, 'INC-42', 'Export outage', 'investigating');
         INSERT INTO outreach_campaigns (id, name, subject, body) VALUES (1, 'Q4 outreach', 'Hello', 'Body');
         INSERT INTO products (id, name, description) VALUES (1, 'Reports widget', 'The reports module');
         INSERT INTO knowledge_sources (id, name) VALUES (1, 'handbook');
         INSERT INTO knowledge_documents (id, source_id, title, visibility) VALUES (1, 1, 'On-call handbook', 'internal_only');
         INSERT INTO graph_edges (source_kind, source_local_id, target_kind, target_local_id, relation, note)
             VALUES ('customer', 11, 'conversation', 1, 'related_to', 'the export ticket');
         INSERT INTO graph_edges (source_kind, source_local_id, target_kind, target_local_id, relation, note)
             VALUES ('known_issue', 1, 'customer', 12, 'depends_on', NULL);
         INSERT INTO graph_edges (source_kind, source_local_id, target_kind, target_local_id, relation, note)
             VALUES ('customer', 11, 'known_issue', 1, 'mentions', 'likely cause');",
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

    // ── GET /api/graph/stats ────────────────────────────────────────────
    let body: Value = client
        .get(format!("{base}/api/graph/stats"))
        .send()
        .await
        .expect("stats")
        .json()
        .await
        .expect("body");
    let nodes = body["nodes"].as_array().expect("nodes");
    assert_eq!(nodes.len(), 12);
    let node_count = |kind: &str| -> i64 {
        nodes
            .iter()
            .find(|n| n["kind"] == json!(kind))
            .unwrap_or_else(|| panic!("{kind} missing"))["count"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(node_count("customer"), 2);
    assert_eq!(node_count("organization"), 1);
    assert_eq!(node_count("conversation"), 1);
    assert_eq!(node_count("known_issue"), 1);
    assert_eq!(node_count("agent"), 1);
    assert_eq!(node_count("incident"), 1);
    assert_eq!(node_count("campaign"), 1);
    assert_eq!(node_count("product"), 1);
    assert_eq!(node_count("knowledge_document"), 1);
    assert_eq!(
        nodes
            .iter()
            .find(|n| n["kind"] == json!("customer"))
            .unwrap()["label"],
        json!("Customer")
    );
    let edges = body["edges"].as_array().expect("edges");
    assert_eq!(edges.len(), 18);
    let edge_count = |relation: &str, origin: &str| -> i64 {
        edges
            .iter()
            .find(|e| e["relation"] == json!(relation) && e["origin"] == json!(origin))
            .unwrap_or_else(|| panic!("{relation}/{origin} missing"))["count"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(edge_count("belongs_to", "helpscout_mirror"), 2);
    assert_eq!(edge_count("involves", "helpscout_mirror"), 1);
    assert_eq!(edge_count("assigned_to", "helpscout_mirror"), 1);
    assert_eq!(edge_count("human_edge", "human_local"), 3);
    assert_eq!(body["human_edges"], json!(3));
    assert!(body["generated_at"].is_string());
    assert_eq!(body["notes"].as_array().unwrap().len(), 3);

    // ── GET /api/graph/search ──────────────────────────────────────────
    let body: Value = client
        .get(format!("{base}/api/graph/search?q=export"))
        .send()
        .await
        .expect("search export")
        .json()
        .await
        .expect("body");
    let results = body["results"].as_array().expect("results");
    let conv = results
        .iter()
        .find(|r| r["kind"] == json!("conversation"))
        .expect("conversation hit");
    assert_eq!(conv["local_id"], json!(1));
    assert_eq!(conv["label"], json!("#101 Export stuck at night"));
    assert_eq!(conv["sublabel"], json!("active"));
    assert_eq!(conv["deleted"], json!(false));
    // The incident also matches on its title.
    assert!(results.iter().any(|r| r["kind"] == json!("incident")));

    // The exact-number conversation match.
    let body: Value = client
        .get(format!("{base}/api/graph/search?q=101"))
        .send()
        .await
        .expect("search number")
        .json()
        .await
        .expect("body");
    assert!(body["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["kind"] == json!("conversation") && r["local_id"] == json!(1)));

    // Customers by name, agents by name/email, organizations, known issues
    // (title through COALESCE), products, docs.
    for (q, kind) in [
        ("ada", "customer"),
        ("dana", "agent"),
        ("acme", "organization"),
        ("login loop", "known_issue"),
        ("widget", "product"),
        ("handbook", "knowledge_document"),
    ] {
        let body: Value = client
            .get(format!("{base}/api/graph/search?q={q}"))
            .send()
            .await
            .expect("search probe")
            .json()
            .await
            .expect("body");
        assert!(
            body["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["kind"] == json!(kind)),
            "q={q} should find a {kind}: {body}"
        );
    }
    let ki = client
        .get(format!("{base}/api/graph/search?q=login loop"))
        .send()
        .await
        .expect("ki search")
        .json::<Value>()
        .await
        .unwrap();
    let ki_row = ki["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == json!("known_issue"))
        .unwrap();
    assert_eq!(ki_row["label"], json!("Login loop after password reset"));

    // The kinds filter narrows.
    let body: Value = client
        .get(format!("{base}/api/graph/search?q=acme&kinds=customer"))
        .send()
        .await
        .expect("kinds filter")
        .json()
        .await
        .expect("body");
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
    let body: Value = client
        .get(format!(
            "{base}/api/graph/search?q=acme&kinds=organization, customer"
        ))
        .send()
        .await
        .expect("kinds filter 2")
        .json()
        .await
        .expect("body");
    assert_eq!(body["results"].as_array().unwrap().len(), 1);

    // An empty query serves no results (the reference returns []).
    let body: Value = client
        .get(format!("{base}/api/graph/search?q="))
        .send()
        .await
        .expect("empty q")
        .json()
        .await
        .expect("body");
    assert_eq!(body["results"].as_array().unwrap().len(), 0);

    // The 422s.
    let r = client
        .get(format!("{base}/api/graph/search?q={}", "x".repeat(201)))
        .send()
        .await
        .expect("long q");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("body");
    assert_eq!(
        body["message"],
        json!("Search query too long (max 200 chars).")
    );
    let r = client
        .get(format!("{base}/api/graph/search?q=x&kinds=banana"))
        .send()
        .await
        .expect("bad kinds");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json().await.expect("body");
    assert_eq!(body["message"], json!("Unknown node kind in kinds filter."));

    // ── GET /api/graph/node/:kind/:id ─────────────────────────────────
    let body: Value = client
        .get(format!("{base}/api/graph/node/customer/11"))
        .send()
        .await
        .expect("node")
        .json()
        .await
        .expect("body");
    assert_eq!(body["node"]["kind"], json!("customer"));
    assert_eq!(body["node"]["local_id"], json!(11));
    assert_eq!(body["node"]["label"], json!("Ada Lovelace"));
    // 4 edges touching Ada: belongs_to (org), involves (conversation),
    // mentions (human), related_to (human).
    assert_eq!(body["edge_count"], json!(4));
    // The 422s and the 404.
    let r = client
        .get(format!("{base}/api/graph/node/banana/11"))
        .send()
        .await
        .expect("bad kind");
    assert_eq!(r.status().as_u16(), 422);
    assert_eq!(
        r.json::<Value>().await.unwrap()["message"],
        json!("Unknown node kind.")
    );
    let r = client
        .get(format!("{base}/api/graph/node/customer/abc"))
        .send()
        .await
        .expect("bad id");
    assert_eq!(r.status().as_u16(), 422);
    assert_eq!(
        r.json::<Value>().await.unwrap()["message"],
        json!("Node id must be a positive integer.")
    );
    let r = client
        .get(format!("{base}/api/graph/node/customer/999"))
        .send()
        .await
        .expect("unknown node");
    assert_eq!(r.status().as_u16(), 404);
    assert_eq!(
        r.json::<Value>().await.unwrap(),
        json!({"statusCode": 404, "error": "NotFound", "message": "Node not found."})
    );

    // ── GET /api/graph/neighbors/:kind/:id ────────────────────────────
    let body: Value = client
        .get(format!("{base}/api/graph/neighbors/customer/11"))
        .send()
        .await
        .expect("neighbors")
        .json()
        .await
        .expect("body");
    assert_eq!(body["node"]["label"], json!("Ada Lovelace"));
    // Derived + human: belongs_to (out, org), involves (in, conversation),
    // mentions (out, human) and related_to (out, human).
    assert_eq!(body["total_edges"], json!(4));
    assert_eq!(body["truncated"], json!(false));
    assert_eq!(body["notes"].as_array().unwrap().len(), 3);
    let edges = body["edges"].as_array().expect("edges");
    assert_eq!(edges.len(), 4);
    // Sorted by relation, then target label:
    // belongs_to < involves < mentions < related_to.
    assert_eq!(edges[0]["relation"], json!("belongs_to"));
    assert_eq!(edges[0]["origin"], json!("helpscout_mirror"));
    assert_eq!(edges[0]["source"]["label"], json!("Ada Lovelace"));
    assert_eq!(edges[0]["target"]["kind"], json!("organization"));
    assert_eq!(edges[0]["target"]["label"], json!("Acme"));
    assert_eq!(edges[0]["note"], json!(null));
    assert_eq!(edges[1]["relation"], json!("involves"));
    assert_eq!(edges[1]["origin"], json!("helpscout_mirror"));
    // Inbound edge: the conversation is the source, Ada the target.
    assert_eq!(
        edges[1]["source"]["label"],
        json!("#101 Export stuck at night")
    );
    assert_eq!(edges[1]["target"]["label"], json!("Ada Lovelace"));
    assert_eq!(edges[2]["relation"], json!("mentions"));
    assert_eq!(edges[2]["source"]["label"], json!("Ada Lovelace"));
    assert_eq!(edges[2]["target"]["kind"], json!("known_issue"));
    assert_eq!(
        edges[2]["target"]["label"],
        json!("Login loop after password reset")
    );
    assert_eq!(edges[2]["origin"], json!("human_local"));
    assert_eq!(edges[2]["note"], json!("likely cause"));
    assert!(edges[2]["at"].is_string());
    assert_eq!(edges[3]["relation"], json!("related_to"));

    // direction=in on customer 11: the derived involves edge from the
    // conversation.
    let body: Value = client
        .get(format!(
            "{base}/api/graph/neighbors/customer/11?direction=in"
        ))
        .send()
        .await
        .expect("direction in")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total_edges"], json!(1));
    assert_eq!(body["edges"].as_array().unwrap().len(), 1);
    assert_eq!(body["edges"][0]["relation"], json!("involves"));
    assert_eq!(
        body["edges"][0]["source"]["label"],
        json!("#101 Export stuck at night")
    );
    assert_eq!(body["edges"][0]["origin"], json!("helpscout_mirror"));

    // The known issue has one of each direction.
    let body: Value = client
        .get(format!("{base}/api/graph/neighbors/known_issue/1"))
        .send()
        .await
        .expect("ki neighbors")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total_edges"], json!(2));
    let body: Value = client
        .get(format!(
            "{base}/api/graph/neighbors/known_issue/1?direction=out"
        ))
        .send()
        .await
        .expect("ki out")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total_edges"], json!(1));
    assert_eq!(body["edges"][0]["relation"], json!("depends_on"));
    assert_eq!(body["edges"][0]["target"]["label"], json!("Belle Node"));
    let body: Value = client
        .get(format!(
            "{base}/api/graph/neighbors/known_issue/1?direction=in"
        ))
        .send()
        .await
        .expect("ki in")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total_edges"], json!(1));
    assert_eq!(body["edges"][0]["source"]["label"], json!("Ada Lovelace"));

    // limit truncation.
    let body: Value = client
        .get(format!("{base}/api/graph/neighbors/customer/11?limit=1"))
        .send()
        .await
        .expect("limit 1")
        .json()
        .await
        .expect("body");
    assert_eq!(body["total_edges"], json!(4));
    assert_eq!(body["edges"].as_array().unwrap().len(), 1);
    assert_eq!(body["truncated"], json!(true));
    assert_eq!(body["notes"].as_array().unwrap().len(), 4);

    // The 422s.
    for (url, message) in [
        (
            "/api/graph/neighbors/customer/11?limit=0",
            "limit must be between 1 and 200.",
        ),
        (
            "/api/graph/neighbors/customer/11?limit=500",
            "limit must be between 1 and 200.",
        ),
        (
            "/api/graph/neighbors/customer/11?direction=sideways",
            "direction must be out, in or both.",
        ),
        ("/api/graph/neighbors/banana/11", "Unknown node kind."),
        (
            "/api/graph/neighbors/customer/3.5",
            "Node id must be a positive integer.",
        ),
    ] {
        let r = client
            .get(format!("{base}{url}"))
            .send()
            .await
            .expect("422 probe");
        assert_eq!(r.status().as_u16(), 422, "{url}");
        assert_eq!(
            r.json::<Value>().await.unwrap()["message"],
            json!(message),
            "{url}"
        );
    }
    // The 404.
    let r = client
        .get(format!("{base}/api/graph/neighbors/customer/999"))
        .send()
        .await
        .expect("unknown neighbors");
    assert_eq!(r.status().as_u16(), 404);

    // ── GET /api/graph/subgraph/:kind/:id ─────────────────────────────
    let body: Value = client
        .get(format!("{base}/api/graph/subgraph/customer/11"))
        .send()
        .await
        .expect("subgraph depth 1")
        .json()
        .await
        .expect("body");
    assert_eq!(body["seeds"][0]["label"], json!("Ada Lovelace"));
    assert_eq!(body["depth_reached"], json!(1));
    assert_eq!(body["truncated"], json!(false));
    let nodes = body["nodes"].as_array().expect("nodes");
    // Seed + organization (belongs_to) + conversation (involves/related_to)
    // + known issue (mentions).
    assert_eq!(nodes.len(), 4);
    assert!(nodes.iter().any(|n| n["kind"] == json!("conversation")));
    assert!(nodes.iter().any(|n| n["kind"] == json!("known_issue")));
    assert!(nodes.iter().any(|n| n["kind"] == json!("organization")));
    assert_eq!(body["edges"].as_array().unwrap().len(), 4);

    // depth=2 reaches Belle through the known issue's depends_on edge.
    let body: Value = client
        .get(format!("{base}/api/graph/subgraph/customer/11?depth=2"))
        .send()
        .await
        .expect("subgraph depth 2")
        .json()
        .await
        .expect("body");
    assert_eq!(body["depth_reached"], json!(2));
    let nodes = body["nodes"].as_array().expect("nodes");
    // Depth 2 adds Belle (org belongs_to / known-issue depends_on) and the
    // assigned agent (conversation assigned_to).
    assert_eq!(nodes.len(), 6);
    assert!(
        nodes
            .iter()
            .any(|n| n["kind"] == json!("customer") && n["local_id"] == json!(12)),
        "Belle should be reached at depth 2"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n["kind"] == json!("agent") && n["local_id"] == json!(7)),
        "Dana (assignee) should be reached at depth 2"
    );
    assert_eq!(
        body["notes"][0],
        json!("Bounded exploration: at most 2 hop(s) and 120 nodes.")
    );
    assert_eq!(body["notes"][1], json!("Full expansion within bounds."));

    // depth 3 is a 422; unknown node a 404.
    let r = client
        .get(format!("{base}/api/graph/subgraph/customer/11?depth=3"))
        .send()
        .await
        .expect("depth 3");
    assert_eq!(r.status().as_u16(), 422);
    assert_eq!(
        r.json::<Value>().await.unwrap()["message"],
        json!("depth must be 1 or 2.")
    );
    let r = client
        .get(format!("{base}/api/graph/subgraph/customer/999"))
        .send()
        .await
        .expect("unknown subgraph");
    assert_eq!(r.status().as_u16(), 404);
}
