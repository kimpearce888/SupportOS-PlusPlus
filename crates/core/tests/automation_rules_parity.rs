//! Audit item AU-01 / blocker B4: automation rule CRUD.
//!
//! B4: the create route INSERTed into nonexistent `trigger`/`action`
//! columns and swallowed the SQL error — POST answered `ok:true` while
//! `automation_rules` stayed empty, so the whole automation surface was
//! unusable. The reference contract (routes/automation.ts:6-88 +
//! engine.ts:34-104 + shared/schemas.ts:499-545) is a zod-validated CRUD
//! with the MAIN trigger/action/condition vocabulary, priority and
//! requires_approval, plus a manual-trigger route that records runs.
//!
//! This test boots the REAL HTTP server (like tests/cors_origins.rs) and
//! live-probes the full CRUD surface:
//!
//!   - GET serves {rules, runs, risk_tiers (static MAIN vocabulary),
//!     automation_enabled};
//!   - POST validates like `automationRuleSchema`: failures answer the 400
//!     envelope with `path: message` details; success persists the rule
//!     (B4 regression check: the row is REALLY in the table), writes the
//!     `automation_rule_created` audit entry and answers "disabled by
//!     default";
//!   - PATCH reproduces the reference semantics: empty patch 422, name /
//!     enabled type checks 422, trigger/conditions/actions patches 404 for
//!     unknown rules and re-validate the MERGED rule against the full
//!     schema (422 ZodError-style envelope);
//!   - DELETE is idempotent-ok;
//!   - the manual trigger route fires matching ENABLED rules: read actions
//!     record `completed` (analyze_ticket enqueues a job), non-destructive
//!     actions execute (add_tag writes the local tag; create_ai_note with
//!     requires_approval parks an approval + `awaiting_approval`), and
//!     higher-risk actions always park + record `awaiting_approval`;
//!     run_count/last_run_at bump; automation disabled ⇒ 0 runs.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PORT: u16 = 3991;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automation_rule_crud_matches_the_reference_contract() {
    // ── Boot, mirroring the Tauri shell (app/src-tauri/src/lib.rs) ──────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    // Fixture: one conversation (local id 9) with a billing tag.
    conn.execute_batch(
        "INSERT INTO conversations (id, remote_id, number, subject, preview, mailbox_id, customer_id, status)
             VALUES (9, 105011, 5012, 'Refund question', 'I want a refund for...', 1, 1, 'active');
         INSERT INTO tags (id, remote_id, name, slug) VALUES (3, 3003, 'billing', 'billing');
         INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (9, 3);",
    )
    .expect("seed fixtures");

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
        data_dir: data_dir.clone(),
        port: PORT,
        host: "127.0.0.1".to_string(),
        demo_mode: true,
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
        assert!(
            Instant::now() < deadline,
            "server did not come up on port {PORT}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // A second connection to the same DB file for table-level assertions.
    let db2 = spp_core::db::open(&db_path).expect("second DB connection");

    // ── 1. GET: empty state + the static MAIN risk-tier vocabulary ─────────
    let r = client
        .get(format!("{base}/api/automation/rules"))
        .send()
        .await
        .expect("list");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json::<Value>().await.expect("list body");
    assert_eq!(body["rules"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["runs"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["automation_enabled"], json!(false));
    assert_eq!(
        body["risk_tiers"]["read"],
        json!(["analyze_ticket", "search_similar", "check_known_issues"])
    );
    assert_eq!(
        body["risk_tiers"]["non_destructive"],
        json!([
            "create_ai_note",
            "create_ai_draft",
            "add_tag",
            "manual_review_queue"
        ])
    );
    assert_eq!(
        body["risk_tiers"]["higher_risk"],
        json!(["set_status", "assign"])
    );
    assert_eq!(
        body["risk_tiers"]["note"].as_str(),
        Some("Higher-risk actions always require explicit approval. Help Scout workflows are a separate system (Automation screen shows both).")
    );

    // ── 2. POST: zod validation failures answer the 400 envelope ───────────
    let r = client
        .post(format!("{base}/api/automation/rules"))
        .json(&json!({
            "name": "",
            "trigger": "status_changed",
            "conditions": [
                { "field": "ai_attribute", "operator": "equals", "value": "high" }
            ],
            "actions": []
        }))
        .send()
        .await
        .expect("invalid create");
    assert_eq!(r.status().as_u16(), 400);
    let body: Value = r.json::<Value>().await.expect("400 body");
    assert_eq!(body["statusCode"], 400);
    assert_eq!(body["error"], "BadRequest");
    assert_eq!(body["message"], "Invalid automation rule.");
    let detail = body["detail"].as_str().expect("detail");
    assert!(
        detail.contains("name: String must contain at least 1 character(s)"),
        "{detail}"
    );
    assert!(detail.contains("trigger: Invalid enum value"), "{detail}");
    assert!(
        detail.contains(
            "conditions.0.attribute: field 'ai_attribute' requires the catalog attribute key (e.g. urgency)."
        ),
        "{detail}"
    );
    assert!(
        detail.contains("actions: Array must contain at least 1 element(s)"),
        "{detail}"
    );

    // ── 3. POST: valid rule persists (the B4 regression) ────────────────────
    let r = client
        .post(format!("{base}/api/automation/rules"))
        .json(&json!({
            "name": "Refund escalation",
            "trigger": "manual",
            "conditions": [
                { "field": "subject", "operator": "contains", "value": "refund" }
            ],
            "actions": [
                { "kind": "analyze_ticket" },
                { "kind": "add_tag", "params": { "tag": "vip" } },
                { "kind": "set_status", "params": { "status": "pending" } }
            ],
            "priority": 10,
            "requires_approval": false
        }))
        .send()
        .await
        .expect("create rule 1");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json::<Value>().await.expect("create body");
    let rule1: i64 = body["id"].as_i64().expect("rule id");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(
        body["message"].as_str(),
        Some("Automation rule created (disabled by default - enable it when ready).")
    );
    // B4 regression: the row is REALLY in the table now.
    let count: i64 = db2
        .query_row("SELECT COUNT(*) FROM automation_rules", [], |r| r.get(0))
        .expect("count rules");
    assert_eq!(count, 1, "the B4 bug left this table empty");

    // The audit entry (reference automation.ts:25).
    let audit_count: i64 = db2
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE actor = 'user'
             AND action = 'automation_rule_created'",
            [],
            |r| r.get(0),
        )
        .expect("count audit");
    assert_eq!(audit_count, 1);

    // A second rule — create_ai_note with requires_approval default true.
    let r = client
        .post(format!("{base}/api/automation/rules"))
        .json(&json!({
            "name": "Note writer",
            "trigger": "manual",
            "actions": [{ "kind": "create_ai_note" }]
        }))
        .send()
        .await
        .expect("create rule 2");
    assert_eq!(r.status().as_u16(), 200);
    let rule2: i64 = r.json::<Value>().await.expect("rule 2 body")["id"]
        .as_i64()
        .expect("id 2");

    // ── 4. GET: MAIN payload shape + defaults + reference ordering ─────────
    let r = client
        .get(format!("{base}/api/automation/rules"))
        .send()
        .await
        .expect("list after create");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json::<Value>().await.expect("list body");
    let rules = body["rules"].as_array().expect("rules array");
    assert_eq!(rules.len(), 2);
    let first = &rules[0]; // priority 10 < 100 → rule1 first (ORDER BY priority, id)
    assert_eq!(first["id"].as_i64(), Some(rule1));
    assert_eq!(first["name"], "Refund escalation");
    assert_eq!(first["enabled"], json!(0), "disabled by default (0|1 int)");
    assert_eq!(first["trigger"], "manual");
    assert_eq!(first["priority"].as_i64(), Some(10));
    assert_eq!(first["requires_approval"], json!(0));
    assert_eq!(first["run_count"].as_i64(), Some(0));
    assert!(first["last_run_at"].is_null());
    assert_eq!(first["conditions"].as_array().map(Vec::len), Some(1));
    assert_eq!(first["conditions"][0]["field"], "subject");
    assert_eq!(first["actions"].as_array().map(Vec::len), Some(3));
    assert_eq!(first["actions"][0]["kind"], "analyze_ticket");
    assert_eq!(
        first["actions"][0]["params"],
        json!({}),
        "params default filled"
    );
    let second = &rules[1];
    assert_eq!(second["id"].as_i64(), Some(rule2));
    assert_eq!(
        second["priority"].as_i64(),
        Some(100),
        "priority default 100"
    );
    assert_eq!(second["requires_approval"], json!(1), "default true");
    assert_eq!(second["conditions"], json!([]));

    // ── 5. PATCH reference semantics ────────────────────────────────────────
    // Empty patch → 422 with the reference message.
    let r = client
        .patch(format!("{base}/api/automation/rules/{rule1}"))
        .json(&json!({ "unknown_key": 1 }))
        .send()
        .await
        .expect("empty patch");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json::<Value>().await.expect("422 body");
    assert_eq!(body["statusCode"], 422);
    assert_eq!(
        body["message"].as_str(),
        Some("No valid fields to update (name, enabled, trigger, conditions, actions, priority, requires_approval).")
    );

    // name-only invalid → 422.
    let r = client
        .patch(format!("{base}/api/automation/rules/{rule1}"))
        .json(&json!({ "name": "   " }))
        .send()
        .await
        .expect("blank name");
    assert_eq!(r.status().as_u16(), 422);
    assert_eq!(
        r.json::<Value>().await.expect("body")["message"].as_str(),
        Some("name must be a non-empty string (max 200 chars).")
    );

    // enabled non-boolean → 422.
    let r = client
        .patch(format!("{base}/api/automation/rules/{rule1}"))
        .json(&json!({ "enabled": "yes" }))
        .send()
        .await
        .expect("bad enabled");
    assert_eq!(r.status().as_u16(), 422);
    assert_eq!(
        r.json::<Value>().await.expect("body")["message"].as_str(),
        Some("enabled must be a boolean.")
    );

    // trigger patch on unknown rule → 404.
    let r = client
        .patch(format!("{base}/api/automation/rules/999999"))
        .json(&json!({ "trigger": "manual" }))
        .send()
        .await
        .expect("unknown rule");
    assert_eq!(r.status().as_u16(), 404);
    let body: Value = r.json::<Value>().await.expect("404 body");
    assert_eq!(body["statusCode"], 404);
    assert_eq!(body["error"], "NotFound");
    assert_eq!(body["message"], "Rule not found.");

    // Valid trigger patch: merged rule re-validated, name preserved.
    let r = client
        .patch(format!("{base}/api/automation/rules/{rule1}"))
        .json(&json!({
            "trigger": "customer_reply",
            "conditions": [{ "field": "tag", "operator": "contains", "value": "billing" }]
        }))
        .send()
        .await
        .expect("valid trigger patch");
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        r.json::<Value>().await.expect("body")["message"].as_str(),
        Some("Rule updated.")
    );
    let r = client
        .get(format!("{base}/api/automation/rules"))
        .send()
        .await
        .expect("list");
    let body: Value = r.json::<Value>().await.expect("body");
    let rule1_json = body["rules"]
        .as_array()
        .expect("rules")
        .iter()
        .find(|r| r["id"].as_i64() == Some(rule1))
        .expect("rule1")
        .clone();
    assert_eq!(rule1_json["trigger"], "customer_reply");
    assert_eq!(
        rule1_json["name"], "Refund escalation",
        "name preserved by merge"
    );
    assert_eq!(rule1_json["conditions"][0]["field"], "tag");
    assert_eq!(rule1_json["actions"].as_array().map(Vec::len), Some(3));

    // Invalid merged candidate → 422 ZodError-style envelope.
    let r = client
        .patch(format!("{base}/api/automation/rules/{rule1}"))
        .json(&json!({ "actions": [{ "kind": "bogus_action" }] }))
        .send()
        .await
        .expect("invalid merged candidate");
    assert_eq!(r.status().as_u16(), 422);
    let body: Value = r.json::<Value>().await.expect("422 body");
    assert_eq!(body["statusCode"], 422);
    assert_eq!(body["error"], "ValidationError");
    let msg = body["message"].as_str().expect("message");
    assert!(
        msg.starts_with("Invalid request (actions.0.kind): Invalid enum value"),
        "got: {msg}"
    );
    let issues = body["issues"].as_array().expect("issues");
    assert!(!issues.is_empty());
    assert_eq!(issues[0]["path"], "actions.0.kind");

    // Restore the manual trigger for the fire tests below (another valid
    // trigger-path patch).
    let r = client
        .patch(format!("{base}/api/automation/rules/{rule1}"))
        .json(&json!({ "trigger": "manual" }))
        .send()
        .await
        .expect("restore trigger");
    assert_eq!(r.status().as_u16(), 200);

    // Enable rule1 (simple path).
    let r = client
        .patch(format!("{base}/api/automation/rules/{rule1}"))
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .expect("enable");
    assert_eq!(r.status().as_u16(), 200);

    // ── 6. DELETE is idempotent-ok ─────────────────────────────────────────
    // (rule2 deleted later, after the fire tests)

    // ── 7. Manual trigger: unknown rule → ok:false ─────────────────────────
    let r = client
        .post(format!("{base}/api/automation/rules/999999/trigger/9"))
        .send()
        .await
        .expect("trigger unknown rule");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json::<Value>().await.expect("body");
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["message"], "Rule not found.");

    // ── 8. Fire with automation disabled → 0 runs ──────────────────────────
    let r = client
        .post(format!("{base}/api/automation/rules/{rule1}/trigger/9"))
        .send()
        .await
        .expect("fire disabled");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json::<Value>().await.expect("body");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(
        body["message"].as_str(),
        Some("Trigger fired (0 runs recorded).")
    );
    assert_eq!(body["runs"].as_array().map(Vec::len), Some(0));

    // ── 9. Enable the engine via the real settings API ─────────────────────
    let r = client
        .patch(format!("{base}/api/settings"))
        .json(&json!({ "automation_enabled": true }))
        .send()
        .await
        .expect("enable automation");
    assert_eq!(
        r.status().as_u16(),
        200,
        "settings patch must accept automation_enabled"
    );

    // ── 10. Fire rule1: read + non-destructive + higher-risk dispatch ─────
    // Enable rule2 too so both manual rules fire (one run row per rule).
    let r = client
        .patch(format!("{base}/api/automation/rules/{rule2}"))
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .expect("enable rule2");
    assert_eq!(r.status().as_u16(), 200);

    let r = client
        .post(format!("{base}/api/automation/rules/{rule1}/trigger/9"))
        .send()
        .await
        .expect("fire rule1");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json::<Value>().await.expect("fire body");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(
        body["message"].as_str(),
        Some("Trigger fired (2 runs recorded)."),
        "one run per fired rule (both manual rules enabled): {body}"
    );
    let runs = body["runs"].as_array().expect("runs");
    // Each entry is the LAST run row of its rule: rule1's last action is
    // set_status (higher_risk → awaiting_approval), rule2's create_ai_note
    // is gated (requires_approval, write actions disabled) → awaiting.
    for run in runs {
        assert_eq!(run["status"], "awaiting_approval", "run: {run}");
        assert!(run["rule_id"].as_i64().is_some());
        assert!(
            run["triggered_at"].as_str().is_some(),
            "MAIN run payload keys"
        );
        assert!(run["detail"].as_str().is_some());
    }

    // The full run log (GET) carries every recorded action of rule1.
    let r = client
        .get(format!("{base}/api/automation/rules"))
        .send()
        .await
        .expect("list with runs");
    let body: Value = r.json::<Value>().await.expect("body");
    let all_runs = body["runs"].as_array().expect("runs");
    assert!(
        all_runs.len() >= 4,
        "rule1 recorded 3 actions + rule2 one: {all_runs:?}"
    );
    let details: Vec<&str> = all_runs
        .iter()
        .filter_map(|r| r["detail"].as_str())
        .collect();
    assert!(
        details.contains(&"Executed read action analyze_ticket"),
        "{details:?}"
    );
    assert!(details.contains(&"Executed add_tag"), "{details:?}");
    assert!(
        details.contains(&"Action set_status is a write action and requires explicit approval"),
        "{details:?}"
    );
    assert!(
        details.contains(&"Action create_ai_note requires approval (non-destructive)"),
        "{details:?}"
    );
    let statuses: Vec<&str> = all_runs
        .iter()
        .filter_map(|r| r["status"].as_str())
        .collect();
    assert!(statuses.contains(&"completed"), "{statuses:?}");
    assert!(statuses.contains(&"awaiting_approval"), "{statuses:?}");

    // run_count / last_run_at bumped.
    let rule1_json = body["rules"]
        .as_array()
        .expect("rules")
        .iter()
        .find(|r| r["id"].as_i64() == Some(rule1))
        .expect("rule1");
    assert_eq!(rule1_json["run_count"].as_i64(), Some(1));
    assert!(
        rule1_json["last_run_at"].as_str().is_some(),
        "last_run_at bumped"
    );

    // ── 11. Side effects: job enqueue, local tag write, parked approvals ───
    let analyze_jobs: i64 = db2
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE type = 'analyze_ticket' AND queue = 'ai'",
            [],
            |r| r.get(0),
        )
        .expect("count analyze jobs");
    assert_eq!(
        analyze_jobs, 1,
        "read action enqueues the analyze_ticket job"
    );

    let tags = spp_core::conversation_ops::read_conversation_tags(&db2, 9);
    assert!(
        tags.iter().any(|t| t.eq_ignore_ascii_case("vip")),
        "add_tag executed locally: {tags:?}"
    );

    // AU-04: gated actions park as `automation_action_awaiting_approval`
    // JOBS (MAIN parity — visible in the Queue panel, approve via retry
    // with {approved:true}, reject via cancel), not automation_approvals
    // rows.
    let approvals: Vec<String> = {
        let mut stmt = db2
            .prepare(
                "SELECT COALESCE(payload, '') FROM jobs
                  WHERE type = 'automation_action_awaiting_approval' AND queue = 'ai'",
            )
            .expect("prepare parked jobs");
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query parked jobs")
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    assert_eq!(
        approvals.len(),
        2,
        "set_status + create_ai_note parked: {approvals:?}"
    );
    assert!(
        approvals.iter().any(|a| a.contains("set_status")),
        "{approvals:?}"
    );
    assert!(
        approvals.iter().any(|a| a.contains("create_ai_note")),
        "{approvals:?}"
    );
    // Each parked job carries the reference retry budget (maxAttempts 1).
    let parked_max: i64 = db2
        .query_row(
            "SELECT COALESCE(MIN(max_attempts), 0) FROM jobs
              WHERE type = 'automation_action_awaiting_approval'",
            [],
            |r| r.get(0),
        )
        .expect("parked max_attempts");
    assert_eq!(parked_max, 1);
    // The legacy approval table is no longer written by the fire path.
    let legacy: i64 = db2
        .query_row("SELECT COUNT(*) FROM automation_approvals", [], |r| {
            r.get(0)
        })
        .expect("count legacy approvals");
    assert_eq!(legacy, 0, "the fire path parks jobs, not approvals");

    // ── 12. Unknown conversation → 0 runs ──────────────────────────────────
    let r = client
        .post(format!(
            "{base}/api/automation/rules/{rule1}/trigger/999999"
        ))
        .send()
        .await
        .expect("fire unknown conversation");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json::<Value>().await.expect("body");
    assert_eq!(
        body["message"].as_str(),
        Some("Trigger fired (0 runs recorded).")
    );

    // ── 13. DELETE ─────────────────────────────────────────────────────────
    let r = client
        .delete(format!("{base}/api/automation/rules/{rule2}"))
        .send()
        .await
        .expect("delete rule2");
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        r.json::<Value>().await.expect("body")["message"].as_str(),
        Some("Rule deleted.")
    );
    let r = client
        .delete(format!("{base}/api/automation/rules/{rule2}"))
        .send()
        .await
        .expect("delete again");
    assert_eq!(
        r.status().as_u16(),
        200,
        "idempotent ok (reference behavior)"
    );
    let r = client
        .get(format!("{base}/api/automation/rules"))
        .send()
        .await
        .expect("final list");
    let body: Value = r.json::<Value>().await.expect("body");
    assert_eq!(body["rules"].as_array().map(Vec::len), Some(1));
}
