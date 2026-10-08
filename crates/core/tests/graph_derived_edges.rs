//! GR-01 verification: the read-time derived edge layer on the real HTTP
//! server — neighbors/subgraph/node now compose the human-asserted store
//! PLUS the ~24 derived branches, each edge carrying its origin label
//! (helpscout_mirror / deterministic_local / ai_derived / human_local).
//!
//! Branches exercised by the seeded world:
//!
//! - belongs_to          customer -> organization        (customers.organization_id)
//! - involves            conversation -> customer        (conversations.customer_id)
//! - assigned_to         conversation -> agent           (conversations.assignee_id)
//! - owns                incident -> agent                (incidents.owner_user_local_id)
//! - linked_to_issue     conversation -> known_issue      (per-row ai/human provenance)
//! - clustered_into      conversation -> issue_cluster   (issue_cluster_conversations)
//! - promoted_to_issue   issue_cluster -> known_issue     (issue_clusters.known_issue_id)
//! - affected_by         conversation -> incident          (incident_conversations)
//! - related_to          incident -> known_issue           (incident_related)
//! - linked_to            custom_object -> conversation    (custom_object_links)
//! - sent_to              campaign -> customer             (outreach_recipients)
//! - generated_conversation campaign -> conversation       (recipients' remote ids)
//! - cites                conversation -> knowledge_document (ai_sources)
//! - collaborated_on      agent -> conversation            (side threads)
//! - about_product        incident/known_issue/cluster -> product (product columns)
//! - about_product        conversation -> product         (ai_attributes)
//! - gap_evidence         knowledge_document -> knowledge_document (gap candidates)

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4023;

#[tokio::test]
async fn graph_derived_edge_layer() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // The derived-layer world: two customers (Ada org'd, Belle not), two
    // conversations (1 assigned to Dana, 2 about the Reports widget), a
    // known issue + cluster both tagged with the product, an incident
    // owned by Grace and related to the issue, two knowledge documents
    // co-cited on a gap candidate, a campaign that sent to Ada and
    // generated conversation 1, a side thread on conversation 2 with Grace
    // as participant, a custom object linked to conversation 1, an AI run
    // on conversation 1 citing document 1, and an AI product attribute on
    // conversation 2.
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO organizations (id, remote_id, name, domains) VALUES (1, 21, 'Acme', '[\"acme.com\"]');
         INSERT INTO users (id, remote_id, first_name, last_name, email) VALUES (7, 70, 'Dana', 'Reyes', 'dana@example.com');
         INSERT INTO users (id, remote_id, first_name, last_name, email) VALUES (8, 80, 'Grace', 'Hopper', 'grace@example.com');
         INSERT INTO customers (id, remote_id, first_name, last_name, organization_id) VALUES (11, 110, 'Ada', 'Lovelace', 1);
         INSERT INTO customers (id, remote_id, first_name, last_name) VALUES (12, 120, 'Belle', 'Node');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, assignee_id)
             VALUES (1, 101, 101, 'Export stuck at night', 'active', 1, 11, 7);
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id)
             VALUES (2, 102, 102, 'Widget question', 'active', 1, 12);
         INSERT INTO products (id, name, description) VALUES (1, 'Reports widget', 'The reports module');
         INSERT INTO known_issues (id, name, status, product) VALUES (1, 'Login loop', 'investigating', 'Reports widget');
         INSERT INTO issue_clusters (id, name, conversation_count, known_issue_id, product)
             VALUES (1, 'night exports', 1, 1, 'Reports widget');
         INSERT INTO incidents (id, code, title, status, owner_user_local_id, product)
             VALUES (1, 'INC-42', 'Export outage', 'investigating', 8, 'Reports widget');
         INSERT INTO knowledge_sources (id, name) VALUES (1, 'handbook');
         INSERT INTO knowledge_documents (id, source_id, title, visibility) VALUES (1, 1, 'On-call handbook', 'internal_only');
         INSERT INTO knowledge_documents (id, source_id, title, visibility) VALUES (2, 1, 'Export runbook', 'internal_only');
         INSERT INTO knowledge_gap_candidates (id, query_text, occurrence_count, related_document_ids)
             VALUES (1, 'how to unblock exports', 3, '[1,2]');
         INSERT INTO known_issue_links (known_issue_id, conversation_id, link_type) VALUES (1, 1, 'ai');
         INSERT INTO known_issue_links (known_issue_id, conversation_id, link_type) VALUES (1, 2, 'human');
         INSERT INTO issue_cluster_conversations (cluster_id, conversation_id) VALUES (1, 1);
         INSERT INTO incident_conversations (incident_id, conversation_id) VALUES (1, 1);
         INSERT INTO incident_related (incident_id, target_kind, target_local_id, note)
             VALUES (1, 'known_issue', 1, 'same login loop');
         INSERT INTO outreach_campaigns (id, name, subject, body, status) VALUES (1, 'Q4 outreach', 'Hello', 'Body', 'completed');
         INSERT INTO outreach_recipients (campaign_id, customer_local_id, email, state, sent_at, hs_conversation_remote_id, hs_conversation_number)
             VALUES (1, 11, 'ada@acme.com', 'sent', '2026-09-25T10:00:00.000Z', 101, 101);
         INSERT INTO side_threads (id, conversation_id, created_at) VALUES (1, 2, '2026-09-20T10:00:00.000Z');
         INSERT INTO side_thread_participants (side_thread_id, user_local_id) VALUES (1, 8);
         INSERT INTO custom_object_types (id, name, slug) VALUES (1, 'Account review', 'account_review');
         INSERT INTO custom_objects (id, type_id, title) VALUES (1, 1, 'Account review');
         INSERT INTO custom_object_links (object_id, target_kind, target_local_id, linked_by, linked_at)
             VALUES (1, 'conversation', 1, 'human', '2026-09-21T10:00:00');
         INSERT INTO ai_runs (id, input_hash, prompt_version, model, response_json, type, status, conversation_id)
             VALUES (1, 'h1', 'v1', 'test-model', '{}', 'analysis', 'succeeded', 1);
         INSERT INTO ai_sources (run_id, source_type, source_id, title) VALUES (1, 'knowledge_document', 1, 'On-call handbook');
         INSERT INTO ai_attributes (conversation_id, attribute, value, value_type, confidence, source, schema_version)
             VALUES (2, 'product', 'Reports widget', 'text', 'high', 'ai', 'v2');",
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

    async fn neighbors(client: &reqwest::Client, base: &str, path: &str) -> Value {
        client
            .get(format!("{base}/api/graph/neighbors/{path}"))
            .send()
            .await
            .expect(path)
            .json()
            .await
            .expect("body")
    }

    let relations = |body: &Value| -> Vec<(String, String, String)> {
        body["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["relation"].as_str().unwrap_or_default().to_string(),
                    e["origin"].as_str().unwrap_or_default().to_string(),
                    format!(
                        "{}:{}",
                        e["target"]["kind"].as_str().unwrap_or_default(),
                        e["target"]["local_id"]
                    ),
                )
            })
            .collect()
    };

    // ── Conversation 1: the widest node — 8 derived edges ───────────────
    let body = neighbors(&client, &base, "conversation/1").await;
    assert_eq!(body["total_edges"], json!(8));
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "affected_by".into(),
                "deterministic_local".into(),
                "incident:1".into()
            ),
            (
                "assigned_to".into(),
                "helpscout_mirror".into(),
                "agent:7".into()
            ),
            (
                "cites".into(),
                "ai_derived".into(),
                "knowledge_document:1".into()
            ),
            (
                "clustered_into".into(),
                "deterministic_local".into(),
                "issue_cluster:1".into()
            ),
            (
                "generated_conversation".into(),
                "helpscout_mirror".into(),
                "conversation:1".into()
            ),
            (
                "involves".into(),
                "helpscout_mirror".into(),
                "customer:11".into()
            ),
            (
                "linked_to".into(),
                "human_local".into(),
                "conversation:1".into()
            ),
            (
                "linked_to_issue".into(),
                "ai_derived".into(),
                "known_issue:1".into()
            ),
        ]
    );
    // The generated_conversation edge points IN from the campaign.
    let generated = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "generated_conversation")
        .unwrap();
    assert_eq!(generated["source"]["kind"], json!("campaign"));
    assert_eq!(generated["target"]["kind"], json!("conversation"));
    // The custom-object link carries its linked_at.
    let linked_to = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "linked_to")
        .unwrap();
    assert_eq!(linked_to["source"]["kind"], json!("custom_object"));
    assert_eq!(linked_to["at"], json!("2026-09-21T10:00:00"));

    // node()'s probe counts the same 8.
    let body: Value = client
        .get(format!("{base}/api/graph/node/conversation/1"))
        .send()
        .await
        .expect("node")
        .json()
        .await
        .expect("body");
    assert_eq!(body["edge_count"], json!(8));

    // ── Conversation 2: about_product (AI) + collaborated_on (in) ───────
    let body = neighbors(&client, &base, "conversation/2").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "about_product".into(),
                "ai_derived".into(),
                "product:1".into()
            ),
            (
                "collaborated_on".into(),
                "human_local".into(),
                "conversation:2".into()
            ),
            (
                "involves".into(),
                "helpscout_mirror".into(),
                "customer:12".into()
            ),
            (
                "linked_to_issue".into(),
                "human_local".into(),
                "known_issue:1".into()
            ),
        ]
    );
    // collaborated_on arrives INBOUND from Grace with the thread timestamp.
    let collab = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "collaborated_on")
        .unwrap();
    assert_eq!(collab["source"]["kind"], json!("agent"));
    assert_eq!(collab["source"]["local_id"], json!(8));
    assert_eq!(collab["target"]["kind"], json!("conversation"));
    assert_eq!(collab["at"], json!("2026-09-20T10:00:00.000Z"));

    // ── Agent 8: collaborated_on out + owns in ──────────────────────────
    let body = neighbors(&client, &base, "agent/8").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "collaborated_on".into(),
                "human_local".into(),
                "conversation:2".into()
            ),
            ("owns".into(), "helpscout_mirror".into(), "agent:8".into()),
        ]
    );
    // owns arrives INBOUND from the incident.
    let owns = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "owns")
        .unwrap();
    assert_eq!(owns["source"]["kind"], json!("incident"));
    assert_eq!(owns["target"]["kind"], json!("agent"));

    // ── Campaign 1: sent_to + generated_conversation out ────────────────
    let body = neighbors(&client, &base, "campaign/1").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "generated_conversation".into(),
                "helpscout_mirror".into(),
                "conversation:1".into()
            ),
            (
                "sent_to".into(),
                "helpscout_mirror".into(),
                "customer:11".into()
            ),
        ]
    );
    let sent_to = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "sent_to")
        .unwrap();
    assert_eq!(sent_to["at"], json!("2026-09-25T10:00:00.000Z"));

    // ── Product 1: four about_product in-edges with split origins ──────
    let body = neighbors(&client, &base, "product/1").await;
    assert_eq!(body["total_edges"], json!(4));
    let rels = relations(&body);
    let deterministic = rels
        .iter()
        .filter(|(r, o, _)| r == "about_product" && o == "deterministic_local")
        .count();
    let ai = rels
        .iter()
        .filter(|(r, o, _)| r == "about_product" && o == "ai_derived")
        .count();
    assert_eq!(deterministic, 3, "incident + known issue + cluster");
    assert_eq!(ai, 1, "the AI attribute on conversation 2");
    let sources: Vec<String> = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            format!(
                "{}:{}",
                e["source"]["kind"].as_str().unwrap_or_default(),
                e["source"]["local_id"]
            )
        })
        .collect();
    assert!(sources.contains(&"incident:1".to_string()));
    assert!(sources.contains(&"known_issue:1".to_string()));
    assert!(sources.contains(&"issue_cluster:1".to_string()));
    assert!(sources.contains(&"conversation:2".to_string()));

    // ── Knowledge documents: cites in + gap_evidence co-citation ───────
    let body = neighbors(&client, &base, "knowledge_document/1").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "cites".into(),
                "ai_derived".into(),
                "knowledge_document:1".into()
            ),
            (
                "gap_evidence".into(),
                "deterministic_local".into(),
                "knowledge_document:2".into()
            ),
        ]
    );
    let body = neighbors(&client, &base, "knowledge_document/2").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![(
            "gap_evidence".into(),
            "deterministic_local".into(),
            "knowledge_document:1".into()
        ),]
    );

    // ── Incident 1: owns/about_product out, affected_by/related_to ─────
    let body = neighbors(&client, &base, "incident/1").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "about_product".into(),
                "deterministic_local".into(),
                "product:1".into()
            ),
            (
                "affected_by".into(),
                "deterministic_local".into(),
                "incident:1".into()
            ),
            ("owns".into(), "helpscout_mirror".into(), "agent:8".into()),
            (
                "related_to".into(),
                "human_local".into(),
                "known_issue:1".into()
            ),
        ]
    );
    let related = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "related_to")
        .unwrap();
    assert_eq!(related["note"], json!("same login loop"));
    // affected_by arrives INBOUND from conversation 1.
    let affected = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "affected_by")
        .unwrap();
    assert_eq!(affected["source"]["kind"], json!("conversation"));
    assert_eq!(affected["source"]["local_id"], json!(1));

    // ── Known issue 1: linked_to_issue in with per-row provenance ─────
    let body = neighbors(&client, &base, "known_issue/1").await;
    assert_eq!(body["total_edges"], json!(5));
    let linked: Vec<&Value> = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["relation"] == "linked_to_issue")
        .collect();
    assert_eq!(linked.len(), 2);
    assert!(linked
        .iter()
        .any(|e| e["source"]["local_id"] == json!(1) && e["origin"] == json!("ai_derived")));
    assert!(linked
        .iter()
        .any(|e| e["source"]["local_id"] == json!(2) && e["origin"] == json!("human_local")));
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "about_product".into(),
                "deterministic_local".into(),
                "product:1".into()
            ),
            (
                "linked_to_issue".into(),
                "ai_derived".into(),
                "known_issue:1".into()
            ),
            (
                "linked_to_issue".into(),
                "human_local".into(),
                "known_issue:1".into()
            ),
            (
                "promoted_to_issue".into(),
                "helpscout_mirror".into(),
                "known_issue:1".into()
            ),
            (
                "related_to".into(),
                "human_local".into(),
                "known_issue:1".into()
            ),
        ]
    );

    // ── Issue cluster 1: promoted_to_issue/about_product out, in from
    // its conversations ──────────────────────────────────────────────────
    let body = neighbors(&client, &base, "issue_cluster/1").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "about_product".into(),
                "deterministic_local".into(),
                "product:1".into()
            ),
            (
                "clustered_into".into(),
                "deterministic_local".into(),
                "issue_cluster:1".into()
            ),
            (
                "promoted_to_issue".into(),
                "helpscout_mirror".into(),
                "known_issue:1".into()
            ),
        ]
    );
    let clustered = body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "clustered_into")
        .unwrap();
    assert_eq!(clustered["source"]["kind"], json!("conversation"));
    assert_eq!(clustered["source"]["local_id"], json!(1));

    // ── Customer 11: belongs_to out, involves/sent_to in ────────────────
    let body = neighbors(&client, &base, "customer/11").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![
            (
                "belongs_to".into(),
                "helpscout_mirror".into(),
                "organization:1".into()
            ),
            (
                "involves".into(),
                "helpscout_mirror".into(),
                "customer:11".into()
            ),
            (
                "sent_to".into(),
                "helpscout_mirror".into(),
                "customer:11".into()
            ),
        ]
    );

    // ── Custom object 1: linked_to out ───────────────────────────────────
    let body = neighbors(&client, &base, "custom_object/1").await;
    let rels = relations(&body);
    assert_eq!(
        rels,
        vec![(
            "linked_to".into(),
            "human_local".into(),
            "conversation:1".into()
        )]
    );

    // ── Organization 1: belongs_to in from both customers ───────────────
    let body = neighbors(&client, &base, "organization/1").await;
    assert_eq!(body["total_edges"], json!(1));
    let belongs = &body["edges"].as_array().unwrap()[0];
    assert_eq!(belongs["relation"], json!("belongs_to"));
    assert_eq!(belongs["source"]["kind"], json!("customer"));
    assert_eq!(belongs["source"]["local_id"], json!(11));

    // ── Subgraph traverses the derived layer ───────────────────────────
    let body: Value = client
        .get(format!("{base}/api/graph/subgraph/conversation/1?depth=2"))
        .send()
        .await
        .expect("subgraph")
        .json()
        .await
        .expect("body");
    assert_eq!(body["depth_reached"], json!(2));
    let kinds: Vec<&str> = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap_or_default())
        .collect();
    // The second hop reaches: incident, agent, issue cluster, known issue,
    // custom object, campaign, customer, knowledge document.
    assert!(kinds.contains(&"incident"));
    assert!(kinds.contains(&"agent"));
    assert!(kinds.contains(&"issue_cluster"));
    assert!(kinds.contains(&"known_issue"));
    assert!(kinds.contains(&"custom_object"));
    assert!(kinds.contains(&"campaign"));
    assert!(kinds.contains(&"customer"));
    assert!(kinds.contains(&"knowledge_document"));
    assert!(
        kinds.contains(&"organization"),
        "third hop through customer 11"
    );
    // Every derived edge carries a non-human origin or the human_local
    // label for the human-asserted provenance tables.
    for e in body["edges"].as_array().unwrap() {
        let origin = e["origin"].as_str().unwrap_or_default();
        assert!(
            [
                "helpscout_mirror",
                "deterministic_local",
                "ai_derived",
                "human_local"
            ]
            .contains(&origin),
            "unexpected origin {origin}"
        );
    }
}
