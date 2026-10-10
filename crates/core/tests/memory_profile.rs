//! ME-01 verification: the composed customer memory profile
//! (GET /api/memory/:customerId) on the real HTTP server.
//!
//! The profile is composed entirely at read time — nine sections plus the
//! per-entry freshness classification, the isolated `quarantined` list and
//! composition notes (the MemoryPanel contract):
//!
//! - `sections` — account / issue_history / outcomes / interaction /
//!   preferences / campaigns / ai_entries / human_entries / context, each
//!   `{section, label, entries}` with entries carrying
//!   `{entry_id, title, value, source, confidence, freshness, evidence,
//!   editable, first_seen_at, last_seen_at}`.
//! - `quarantined` — red-line rows isolated from every usable section
//!   (`{entry_id, key, reason}`), purgable through the delete route.
//! - `notes` — composition notes ("Composed from N conversations." ...).
//! - `freshness` — thresholds, the per-entry state histogram, the rated
//!   conversation count and the generation timestamp.
//! - `entries` — the flat usable list kept for direct consumers.
//!
//! Error envelopes: 422 on a non-positive-integer id, 404 on an unknown
//! customer.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SERVER_PORT: u16 = 4022;

#[tokio::test]
async fn memory_profile_composition() {
    // ── Boot the real server on a seeded database ──────────────────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // The mirror world: org, two customers (Ada 11 = the profile target,
    // Belle 12 = the other one), three Ada conversations (two closed with
    // ratings, one active), a known issue + a cluster linked to Ada's
    // conversations, an outreach campaign touching Ada, an interaction
    // baseline row, and a memory set covering every section: human entry,
    // AI entry with evidence/conversation, an outdated entry, and two
    // red-line quarantined rows.
    conn.execute_batch(
        "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
         INSERT INTO organizations (id, remote_id, name, domains) VALUES (1, 21, 'Acme', '[\"acme.com\"]');
         INSERT INTO users (id, remote_id, first_name, last_name, email) VALUES (7, 70, 'Dana', 'Reyes', 'dana@example.com');
         INSERT INTO customers (id, remote_id, first_name, last_name, email, organization_id, job_title)
             VALUES (11, 110, 'Ada', 'Lovelace', 'ada@acme.com', 1, 'Chief Mathematician');
         INSERT INTO customers (id, remote_id, first_name, last_name, organization_id)
             VALUES (12, 120, 'Belle', 'Node', 1);
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id, created_at, updated_at)
             VALUES (1, 101, 101, 'Export stuck at night', 'closed', 1, 11, 7,
                     '2026-09-01T10:00:00.000Z', '2026-09-02T10:00:00.000Z');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id, created_at, updated_at)
             VALUES (2, 102, 102, 'Login loop', 'closed', 1, 11, 7,
                     '2026-09-10T10:00:00.000Z', '2026-09-11T10:00:00.000Z');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id, created_at, updated_at)
             VALUES (3, 103, 103, 'Follow-up question', 'active', 1, 11, 7,
                     '2026-10-01T10:00:00.000Z', '2026-10-02T10:00:00.000Z');
         INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, assignee_local_id)
             VALUES (4, 104, 104, 'Belle own thread', 'active', 1, 12, 7);
         INSERT INTO ratings (conversation_id, customer_local_id, rating, comments, remote_created_at)
             VALUES (1, 11, 'great', 'Fixed overnight', '2026-09-02T11:00:00.000Z');
         INSERT INTO ratings (conversation_id, customer_local_id, rating, comments, remote_created_at)
             VALUES (2, 11, 'not_good', 'Took three days', '2026-09-11T11:00:00.000Z');
         INSERT INTO known_issues (id, name, status, title)
             VALUES (1, 'Login loop', 'investigating', 'Login loop after password reset');
         INSERT INTO known_issue_links (known_issue_id, conversation_id, link_type)
             VALUES (1, 2, 'related');
         INSERT INTO issue_clusters (id, name, conversation_count) VALUES (1, 'night exports', 2);
         INSERT INTO issue_cluster_conversations (cluster_id, conversation_id) VALUES (1, 1);
         INSERT INTO outreach_campaigns (id, name, subject, body, status) VALUES (1, 'Q4 outreach', 'Hello', 'Body', 'completed');
         INSERT INTO outreach_recipients (campaign_id, customer_local_id, email, state, sent_at, replied_at)
             VALUES (1, 11, 'ada@acme.com', 'sent', '2026-09-25T10:00:00.000Z', '2026-09-26T10:00:00.000Z');
         INSERT INTO client_behavior_baselines (customer_id, dimension, typical_value, confidence, observation_count, last_observed)
             VALUES (11, 'response_preference', 'concise', 'medium', 3, '2026-09-28T10:00:00.000Z');
         INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt,
                                      source_conversation_id, source, origin, confidence,
                                      provenance, kind, first_seen_at, last_seen_at, created_at)
             VALUES (11, 'preferred_channel', 'email', 'asked to keep replies on email',
                     1, 'ai', 'conversation', 'high', 'ai_generated', 'preference',
                     '2026-09-01T11:00:00.000Z', '2026-10-01T11:00:00.000Z', '2026-09-15T11:00:00.000Z');
         INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source,
                                      origin, confidence, provenance, kind, first_seen_at, last_seen_at, created_at)
             VALUES (11, 'team_size', '40 engineers', 'stated in onboarding call', 'human', 'manual', 'high', 'human_local',
                     'account', '2026-08-01T11:00:00.000Z', '2026-08-05T11:00:00.000Z', '2026-08-05T11:00:00.000Z');
         INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source,
                                      kind, first_seen_at, last_seen_at, created_at)
             VALUES (11, 'legacy_export_pref', 'weekly digest', 'legacy import', 'ai', 'history',
                     '2024-01-01T11:00:00.000Z', '2024-01-02T11:00:00.000Z', '2024-01-02T11:00:00.000Z');
         INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source, created_at)
             VALUES (11, 'customer_personality', 'very introverted', 'red-line row', 'human', '2026-09-20T11:00:00.000Z');
         INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source, kind, created_at)
             VALUES (11, 'mood_disorder_suspected', 'possible mood disorder — handle with care', 'red-line row', 'ai', 'context', '2026-09-21T11:00:00.000Z');
         INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source)
             VALUES (12, 'belle_note', 'unrelated row', 'other customer', 'human');",
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

    // ── GET /api/memory/11 — the composed profile ──────────────────────
    let resp = client
        .get(format!("{base}/api/memory/11"))
        .send()
        .await
        .expect("profile request");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("profile body");

    assert_eq!(body["customerId"], json!(11));

    // Nine sections, in the canonical order.
    let sections = body["sections"].as_array().expect("sections array");
    assert_eq!(sections.len(), 9);
    let section_keys: Vec<&str> = sections
        .iter()
        .map(|s| s["section"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        section_keys,
        vec![
            "account",
            "issue_history",
            "outcomes",
            "interaction",
            "preferences",
            "campaigns",
            "ai_entries",
            "human_entries",
            "context",
        ]
    );
    let find_section = |key: &str| {
        sections
            .iter()
            .find(|s| s["section"] == json!(key))
            .unwrap()
    };

    // Section 1: account — mirror identity + the kind=account human row.
    let account = find_section("account");
    assert_eq!(account["label"], json!("Account facts"));
    let account_entries = account["entries"].as_array().unwrap();
    assert!(account_entries
        .iter()
        .any(|e| e["title"] == json!("Organization")
            && e["value"] == json!("Acme")
            && e["source"] == json!("helpscout_mirror")
            && e["editable"] == json!(false)));
    assert!(account_entries
        .iter()
        .any(|e| e["title"] == json!("Job title") && e["value"] == json!("Chief Mathematician")));
    assert!(account_entries
        .iter()
        .any(|e| e["title"] == json!("team_size") && e["source"] == json!("human_local")));

    // Section 2: issue history — the known issue + the cluster on Ada's
    // conversations.
    let issue_history = find_section("issue_history");
    let ih_entries = issue_history["entries"].as_array().unwrap();
    assert_eq!(ih_entries.len(), 2);
    assert!(ih_entries.iter().any(|e| e["title"]
        .as_str()
        .unwrap()
        .starts_with("Login loop after password reset")
        && e["value"] == json!("Affects 1 of this customer's conversations.")));
    assert!(ih_entries
        .iter()
        .any(|e| e["title"] == json!("Cluster: night exports")));

    // Section 3: outcomes — the two closed conversations with their
    // ratings; the active one is not an outcome.
    let outcomes = find_section("outcomes");
    let out_entries = outcomes["entries"].as_array().unwrap();
    assert_eq!(out_entries.len(), 2);
    assert!(out_entries.iter().any(|e| e["title"]
        .as_str()
        .unwrap()
        .starts_with("#101 Export stuck at night")
        && e["value"].as_str().unwrap().contains("rated great")));
    assert!(out_entries.iter().any(|e| e["title"]
        .as_str()
        .unwrap()
        .starts_with("#102 Login loop")
        && e["value"].as_str().unwrap().contains("rated not_good")));
    assert!(out_entries
        .iter()
        .all(|e| e["source"] == json!("helpscout_mirror")));

    // Section 4: interaction — the baseline dimension row.
    let interaction = find_section("interaction");
    let int_entries = interaction["entries"].as_array().unwrap();
    assert_eq!(int_entries.len(), 1);
    assert_eq!(int_entries[0]["title"], json!("response preference"));
    assert_eq!(int_entries[0]["value"], json!("concise"));
    assert_eq!(int_entries[0]["confidence"], json!("medium"));
    assert_eq!(int_entries[0]["freshness"], json!("fresh"));

    // Section 5: preferences — the kind=preference AI row with evidence.
    let preferences = find_section("preferences");
    let pref_entries = preferences["entries"].as_array().unwrap();
    assert_eq!(pref_entries.len(), 1);
    assert_eq!(pref_entries[0]["title"], json!("preferred_channel"));
    assert_eq!(pref_entries[0]["value"], json!("email"));
    assert_eq!(pref_entries[0]["source"], json!("ai_derived"));
    assert_eq!(pref_entries[0]["confidence"], json!("high"));
    assert_eq!(pref_entries[0]["freshness"], json!("fresh"));
    let evidence = pref_entries[0]["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        evidence[0]["description"],
        json!("asked to keep replies on email")
    );
    assert_eq!(evidence[0]["conversation_number"], json!(101));
    assert_eq!(pref_entries[0]["editable"], json!(false));

    // Section 6: campaigns — the outreach touch with the replied state.
    let campaigns = find_section("campaigns");
    let camp_entries = campaigns["entries"].as_array().unwrap();
    assert_eq!(camp_entries.len(), 1);
    assert_eq!(camp_entries[0]["title"], json!("Q4 outreach"));
    assert_eq!(camp_entries[0]["value"], json!("Sent — customer replied."));
    assert_eq!(camp_entries[0]["freshness"], json!("fresh"));

    // Section 7: ai_entries — the AI fact/history rows (not preference,
    // not context).
    let ai_entries = find_section("ai_entries");
    let ai_list = ai_entries["entries"].as_array().unwrap();
    assert_eq!(ai_list.len(), 1);
    assert_eq!(ai_list[0]["title"], json!("legacy_export_pref"));
    assert_eq!(ai_list[0]["freshness"], json!("stale"));

    // Section 8: human_entries — editable, with the human source.
    let human = find_section("human_entries");
    let human_list = human["entries"].as_array().unwrap();
    assert_eq!(human_list.len(), 1);
    assert_eq!(human_list[0]["title"], json!("team_size"));
    assert_eq!(human_list[0]["source"], json!("human_local"));
    assert_eq!(human_list[0]["editable"], json!(true));
    assert!(human_list[0]["entry_id"].is_i64());

    // Section 9: context — empty here (the only AI context row is
    // quarantined), but the section itself is present.
    let context = find_section("context");
    assert_eq!(context["entries"].as_array().unwrap().len(), 0);

    // Quarantined — the two red-line rows, isolated from every section,
    // with the purgeable entry ids.
    let quarantined = body["quarantined"].as_array().expect("quarantined array");
    assert_eq!(quarantined.len(), 2);
    assert!(quarantined
        .iter()
        .any(|q| q["key"] == json!("customer_personality") && q["entry_id"].is_i64()));
    assert!(quarantined
        .iter()
        .any(|q| q["key"] == json!("mood_disorder_suspected")));
    for q in quarantined {
        assert!(q["reason"].as_str().unwrap().contains("policy"));
    }
    for s in sections {
        for e in s["entries"].as_array().unwrap_or(&Vec::new()) {
            let title = e["title"].as_str().unwrap_or_default();
            assert!(
                !title.contains("personality") && !title.contains("mood"),
                "quarantined row leaked into section {title}"
            );
        }
    }

    // Notes + freshness summary.
    let notes = body["notes"].as_array().expect("notes array");
    assert_eq!(notes[0], json!("Composed from 3 conversations."));
    assert!(notes
        .iter()
        .any(|n| n.as_str().unwrap().starts_with("2 quarantined")));
    let freshness = &body["freshness"];
    assert_eq!(freshness["thresholds_days"]["fresh"], json!(30));
    assert_eq!(freshness["thresholds_days"]["aging"], json!(90));
    assert_eq!(freshness["rated_conversations"], json!(2));
    assert!(freshness["generated_at"].as_str().is_some());
    let states = &freshness["entry_states"];
    let hist_total = states["fresh"].as_i64().unwrap()
        + states["aging"].as_i64().unwrap()
        + states["stale"].as_i64().unwrap()
        + states["unknown"].as_i64().unwrap();
    assert!(
        hist_total >= 7,
        "expected the histogram to cover the composed entries"
    );
    assert_eq!(freshness["total_section_entries"], json!(hist_total));

    // The flat compat list: usable rows only (3 for Ada).
    let entries = body["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 3);

    // ── The 422 on a non-integer id ────────────────────────────────────
    let resp = client
        .get(format!("{base}/api/memory/not-a-number"))
        .send()
        .await
        .expect("bad id request");
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().await.expect("422 body");
    assert_eq!(body["statusCode"], json!(422));
    assert_eq!(body["error"], json!("ValidationError"));

    // ── The 404 on an unknown customer ─────────────────────────────────
    let resp = client
        .get(format!("{base}/api/memory/9999"))
        .send()
        .await
        .expect("unknown customer request");
    assert_eq!(resp.status().as_u16(), 404);
    let body: Value = resp.json().await.expect("404 body");
    assert_eq!(body["statusCode"], json!(404));
    assert_eq!(body["error"], json!("NotFound"));
    assert_eq!(body["message"], json!("Customer not found."));
}
