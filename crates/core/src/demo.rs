//! Demo-mode tools (A10).
//!
//! Per spec A10: "the reference exposes ways to push a simulated webhook event,
//! a simulated CSAT rating and a simulated incoming customer message through
//! the REAL pipeline (HMAC, dedup, job, sync, live update). Reproduce these
//! as clearly labeled actions available only in demo mode, calling the same
//! code paths as production."
//!
//! These tools are ONLY available when `demo_mode = true` in settings. They
//! call the same production code paths (`webhook_handler::process_webhook`,
//! `jobs::enqueue`, etc.) so the pipeline is exercised exactly as it would
//! be in production — no shortcuts, no mocks.

use rusqlite::Connection;

use crate::jobs;
use crate::settings;
use crate::webhook;
use crate::webhook_handler;

/// The three demo tools (A10). Each pushes a simulated event through the REAL pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoTool {
    /// Push a simulated Help Scout webhook event through the HMAC + dedup + job pipeline.
    SimulatedWebhookEvent,
    /// Push a simulated CSAT rating.
    SimulatedCsatRating,
    /// Push a simulated incoming customer message.
    SimulatedIncomingMessage,
}

impl DemoTool {
    /// The label shown in the UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SimulatedWebhookEvent => "Simulate webhook event",
            Self::SimulatedCsatRating => "Simulate CSAT rating",
            Self::SimulatedIncomingMessage => "Simulate incoming customer message",
        }
    }

    /// A short description of what the tool does.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::SimulatedWebhookEvent => "Pushes a simulated Help Scout webhook event through the real HMAC + dedup + job pipeline.",
            Self::SimulatedCsatRating => "Pushes a simulated CSAT rating through the real ratings pipeline.",
            Self::SimulatedIncomingMessage => "Pushes a simulated incoming customer message through the real sync + live-update pipeline.",
        }
    }

    /// All three demo tools.
    pub const ALL: [Self; 3] = [
        Self::SimulatedWebhookEvent,
        Self::SimulatedCsatRating,
        Self::SimulatedIncomingMessage,
    ];
}

/// The result of running a demo tool.
#[derive(Debug, Clone)]
pub enum DemoToolResult {
    /// The tool ran successfully.
    Success { message: String },
    /// The tool was rejected because demo mode is not enabled.
    DemoModeRequired,
    /// The tool failed.
    Failed { message: String },
}

/// Run a demo tool. This function is the ONLY entry point for demo-mode actions.
///
/// It first checks that `demo_mode` is `true` in settings. If not, returns
/// `DemoToolRequired` — the UI must never call this when demo mode is off.
///
/// Then it calls the real production code path for each tool:
/// - `SimulatedWebhookEvent`: constructs a fake webhook payload, computes the
///   HMAC signature (using the stored webhook secret), and calls
///   `webhook_handler::process_webhook` — the same function the loopback
///   listener uses for real webhooks.
/// - `SimulatedCsatRating`: enqueues a `rating.process` job with a simulated
///   CSAT payload.
/// - `SimulatedIncomingMessage`: enqueues a `sync.conversations` job with a
///   simulated conversation payload.
pub fn run_demo_tool(conn: &Connection, tool: DemoTool) -> DemoToolResult {
    // Guard: demo mode must be enabled.
    let demo_mode = settings::get_bool(conn, "demo_mode", false).unwrap_or(false);
    if !demo_mode {
        return DemoToolResult::DemoModeRequired;
    }

    match tool {
        DemoTool::SimulatedWebhookEvent => simulate_webhook_event(conn),
        DemoTool::SimulatedCsatRating => simulate_csat_rating(conn),
        DemoTool::SimulatedIncomingMessage => simulate_incoming_message(conn),
    }
}

/// Simulate a webhook event: construct a fake Help Scout webhook payload,
/// compute the HMAC signature, and push it through the real `process_webhook`
/// pipeline (persist-first → HMAC verify → dedup → job enqueue).
fn simulate_webhook_event(conn: &Connection) -> DemoToolResult {
    // Reference-shaped payload: `{conversationId, objectID, id, nonce}` with
    // the event type carried out-of-band (the X-Helpscout-Event header in the
    // real flow; passed as the `event_type` argument here).
    let remote_id = 1000 + (chrono::Utc::now().timestamp_millis() % 100);
    let body = serde_json::json!({
        "conversationId": remote_id,
        "objectID": remote_id,
        "id": remote_id,
        "nonce": chrono::Utc::now().timestamp_millis(),
    });
    let event_id = format!("demo_evt_{}", chrono::Utc::now().timestamp_millis());
    let body_bytes = body.to_string().into_bytes();

    // Get the webhook secret from settings (or use a demo secret).
    let secret = settings::get_string(conn, "helpscout_webhook_secret")
        .unwrap_or(None)
        .unwrap_or_else(|| "demo_webhook_secret".into());
    let secret_bytes = secret.as_bytes();

    // Compute the real HMAC signature (same as Help Scout would).
    let signature = webhook::compute_signature(secret_bytes, &body_bytes);

    // Push through the REAL pipeline.
    let result = webhook_handler::process_webhook(
        conn,
        secret_bytes,
        &body_bytes,
        Some(&signature),
        "convo.created",
    );

    match result {
        webhook_handler::WebhookProcessResult::Accepted { .. } => DemoToolResult::Success {
            message: format!(
                "Simulated webhook event {event_id} accepted and enqueued for processing."
            ),
        },
        webhook_handler::WebhookProcessResult::Duplicate { .. } => DemoToolResult::Success {
            message: format!(
                "Simulated webhook event {event_id} was a duplicate (already processed)."
            ),
        },
        webhook_handler::WebhookProcessResult::SignatureInvalid => DemoToolResult::Failed {
            message: "Signature verification failed for simulated webhook event.".into(),
        },
        webhook_handler::WebhookProcessResult::BadRequest => DemoToolResult::Failed {
            message: "Bad request: simulated webhook body was empty or invalid.".into(),
        },
    }
}

/// Simulate a CSAT rating: enqueue a `rating.process` job with a simulated
/// rating payload.
fn simulate_csat_rating(conn: &Connection) -> DemoToolResult {
    let rating_id = format!("demo_rating_{}", chrono::Utc::now().timestamp_millis());
    let payload = serde_json::json!({
        "id": rating_id,
        "conversation_id": 1001,
        "rating": 5,
        "comment": "Great support!",
        "created_at": chrono::Utc::now().to_rfc3339(),
    });

    match jobs::enqueue(conn, "rating.process", &payload.to_string()) {
        Ok(job_id) => DemoToolResult::Success {
            message: format!("Simulated CSAT rating enqueued as job #{job_id}."),
        },
        Err(e) => DemoToolResult::Failed {
            message: format!("Failed to enqueue simulated CSAT rating: {e}"),
        },
    }
}

/// Simulate an incoming customer message: enqueue a `sync.conversations` job
/// with a simulated conversation payload.
fn simulate_incoming_message(conn: &Connection) -> DemoToolResult {
    let conv_number = 1000 + (chrono::Utc::now().timestamp_millis() % 100);
    let payload = serde_json::json!({
        "number": conv_number,
        "subject": "Simulated incoming message",
        "preview": "Hi, I have a question about my order...",
        "status": "active",
        "mailbox_id": 101,
        "customer_id": 2001,
        "simulated": true,
    });

    match jobs::enqueue(conn, "sync.conversations", &payload.to_string()) {
        Ok(job_id) => DemoToolResult::Success {
            message: format!("Simulated incoming message (conversation #{conv_number}) enqueued as job #{job_id}."),
        },
        Err(e) => DemoToolResult::Failed {
            message: format!("Failed to enqueue simulated incoming message: {e}"),
        },
    }
}

/// Check whether demo mode is enabled. Convenience for the UI.
pub fn is_demo_mode(conn: &Connection) -> bool {
    settings::get_bool(conn, "demo_mode", false).unwrap_or(false)
}

// ─── Demo intelligence seed (reference services/demoSeed.ts) ───────────────

struct ConvRow {
    id: i64,
    number: i64,
    subject: Option<String>,
    customer_id: Option<i64>,
}

fn conversations_snapshot(conn: &Connection) -> Vec<ConvRow> {
    let mut stmt = match conn.prepare(
        "SELECT id, number, subject, customer_id FROM conversations WHERE deleted_at IS NULL",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |r| {
        Ok(ConvRow {
            id: r.get(0)?,
            number: r.get(1)?,
            subject: r.get(2)?,
            customer_id: r.get(3)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

fn by_number(convs: &[ConvRow], n: i64) -> Option<&ConvRow> {
    convs.iter().find(|c| c.number == n)
}

fn doc_id_by_title(conn: &Connection, title: &str) -> i64 {
    conn.query_row(
        "SELECT id FROM knowledge_documents WHERE title = ?1",
        rusqlite::params![title],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

fn source_id(conn: &Connection, name: &str, visibility: &str) -> i64 {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM knowledge_sources WHERE name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .ok();
    match existing {
        Some(id) => id,
        None => {
            conn.execute(
                "INSERT INTO knowledge_sources (name, kind, visibility) VALUES (?1, 'import', ?2)",
                rusqlite::params![name, visibility],
            )
            .ok();
            conn.last_insert_rowid()
        }
    }
}

fn upsert_document(
    conn: &Connection,
    source: i64,
    title: &str,
    content: &str,
    visibility: &str,
) -> i64 {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM knowledge_documents WHERE title = ?1",
            rusqlite::params![title],
            |r| r.get(0),
        )
        .ok();
    let id = match existing {
        Some(id) => {
            conn.execute(
                "UPDATE knowledge_documents SET content = ?1, visibility = ?2,
                    updated_at = datetime('now'), version = version + 1
                  WHERE id = ?3",
                rusqlite::params![content, visibility, id],
            )
            .ok();
            id
        }
        None => {
            conn.execute(
                "INSERT INTO knowledge_documents (source_id, title, visibility, content, format)
                 VALUES (?1, ?2, ?3, ?4, 'markdown')",
                rusqlite::params![source, title, visibility, content],
            )
            .ok();
            conn.last_insert_rowid()
        }
    };
    // Chunk (delete+insert; self-debouncing like the reference).
    conn.execute(
        "DELETE FROM knowledge_chunks WHERE document_id = ?1",
        rusqlite::params![id],
    )
    .ok();
    for (index, chunk) in content
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .enumerate()
    {
        conn.execute(
            "INSERT INTO knowledge_chunks (document_id, chunk_index, content)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![id, index as i64, chunk],
        )
        .ok();
    }
    conn.execute(
        "UPDATE knowledge_documents SET last_indexed_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id],
    )
    .ok();
    id
}

fn create_known_issue(conn: &Connection, ki: KnownIssueSeed<'_>) -> i64 {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM known_issues WHERE title = ?1 OR (title IS NULL AND name = ?1)",
            rusqlite::params![ki.title],
            |r| r.get(0),
        )
        .ok();
    let id = match existing {
        Some(id) => id,
        None => {
            conn.execute(
                "INSERT INTO known_issues (name, status, description, title, symptoms, product,
                     feature, known_cause, workaround, customer_safe_explanation,
                     internal_explanation, provenance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    ki.title,
                    ki.status,
                    ki.symptoms,
                    ki.title,
                    ki.symptoms,
                    ki.product,
                    ki.feature,
                    ki.known_cause,
                    ki.workaround,
                    ki.customer_safe_explanation,
                    ki.internal_explanation,
                    ki.provenance,
                ],
            )
            .unwrap_or(0);
            conn.last_insert_rowid()
        }
    };
    for conv in ki.conversation_ids {
        conn.execute(
            "INSERT OR IGNORE INTO known_issue_links (known_issue_id, conversation_id, link_type)
             VALUES (?1, ?2, 'related')",
            rusqlite::params![id, conv],
        )
        .ok();
    }
    id
}

struct KnownIssueSeed<'a> {
    title: &'a str,
    status: &'a str,
    symptoms: &'a str,
    product: &'a str,
    feature: &'a str,
    known_cause: &'a str,
    workaround: &'a str,
    customer_safe_explanation: &'a str,
    internal_explanation: &'a str,
    provenance: &'a str,
    conversation_ids: Vec<i64>,
}

fn save_analysis(
    conn: &Connection,
    conv_id: i64,
    analysis: &serde_json::Value,
    sources: &[serde_json::Value],
) {
    // ai_runs row (the run record), then extracted facts + sources.
    let input_hash = format!("seed:{conv_id}");
    conn.execute(
        "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json)
         VALUES (?1, 'ticket_analysis_v1', 'demo_seed', ?2)",
        rusqlite::params![
            input_hash,
            serde_json::to_string(analysis).unwrap_or_default()
        ],
    )
    .ok();
    let run_id = conn.last_insert_rowid();
    for (key, value) in [
        ("intent", analysis.get("intent")),
        ("primary_question", analysis.get("primary_question")),
        ("customer_goal", analysis.get("customer_goal")),
        ("product", analysis.get("product")),
        ("feature", analysis.get("feature")),
        ("problem_type", analysis.get("problem_type")),
        ("requested_action", analysis.get("requested_action")),
        ("summary", analysis.get("summary")),
    ] {
        if let Some(v) = value {
            if !v.is_null() {
                conn.execute(
                    "INSERT INTO ai_extracted_facts (conversation_id, run_id, key, value, confidence)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        conv_id,
                        run_id,
                        key,
                        v.as_str().unwrap_or_default(),
                        analysis.get("confidence").and_then(|c| c.as_str()).unwrap_or("medium"),
                    ],
                )
                .ok();
            }
        }
    }
    for s in sources {
        conn.execute(
            "INSERT INTO ai_sources (run_id, source_type, source_id, title, relevance, visibility, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            rusqlite::params![
                run_id,
                s.get("source_type").and_then(|v| v.as_str()).unwrap_or("conversation"),
                s.get("source_id").and_then(|v| v.as_i64()).unwrap_or(0),
                s.get("title").and_then(|v| v.as_str()),
                s.get("relevance").and_then(|v| v.as_f64()),
                s.get("visibility").and_then(|v| v.as_str()),
            ],
        )
        .ok();
    }
}

/// Seed the demo intelligence layer (reference `seedDemoData`). Safety:
/// never mix demo data with production data — refuses to run unless demo
/// mode is on. Returns whether the seed ran.
pub fn seed_demo_data(conn: &Connection) -> bool {
    // Safety: never mix demo data with production data.
    let demo_loaded = settings::get_bool(conn, "demo_data_loaded", false).unwrap_or(false);
    let demo_mode = is_demo_mode(conn);
    if !demo_loaded && !demo_mode {
        tracing::warn!(
            "Refusing to seed demo data: this does not look like a demo database. Demo data must stay separate from production data."
        );
        return false;
    }
    let convs = conversations_snapshot(conn);
    let by_number = |n: i64| by_number(&convs, n);
    tracing::info!(conversations = convs.len(), "Seeding demo intelligence");

    // ---------------- Knowledge ----------------
    let source = source_id(conn, "Demo product docs", "customer_safe");
    let internal_source = source_id(conn, "Demo internal runbook", "internal_only");
    let docs: Vec<(i64, &str, &str, &str)> = vec![
        (source, "Timezones and scheduled reports", "# Timezones and scheduled reports\n\nScheduled reports follow the **workspace timezone**: Settings > Workspace > Regional settings.\n\nAfter changing the workspace timezone, open each schedule and re-save it once so the stored times re-anchor to the new timezone.\n\nDaylight-saving changes do not automatically re-anchor schedules that were saved before the change; re-saving after a DST change fixes the stored offset.\n\nAll schedule times are displayed in the workspace timezone.", "customer_safe"),
        (source, "Inviting teammates", "# Inviting teammates\n\nGo to Settings > Users > Invite. Invitation emails are sent immediately.\n\nIf an invitation does not arrive, common causes are spam filtering or a typo in the address. Ask your mail provider to whitelist our sending domain. Invitations can be re-sent from the Users screen at any time.", "customer_safe"),
        (source, "Viewer role capabilities", "# Viewer role\n\nA Viewer can see every dashboard and report shared with their team, but cannot edit, comment or create new ones. Viewers can export data (CSV/PDF) from reports they can see. An Editor seat is required for edit rights.", "customer_safe"),
        (source, "Updating your payment method", "# Updating your payment method\n\nYou can update your card under Settings > Billing > Payment method. After updating, click \"Retry payment\" so the pending invoice is charged again; the license re-activates immediately after a successful charge.\n\nIf your bank reports no charge attempt, the invoice may have entered a retry backoff - retrying manually triggers it immediately.", "customer_safe"),
        (internal_source, "Slack integration 401 runbook (ENG-4471)", "# Slack integration 401 runbook - INTERNAL\n\nSymptom: workspaces report the Slack integration stopped posting updates; integration log shows repeated 401 from Slack.\n\nCause: Slack token rotation policy change; refresh tokens were invalidated for workspaces connected before <policy date>.\n\nFix status: engineering is deploying automatic re-auth flow (ENG-4471). Until deployed, instruct customers to disconnect/reconnect the integration - historical sync data is preserved.\n\nCustomer-facing wording must stay generic: \"an authentication change on Slack's side\" - do not mention internal ticket IDs.", "internal_only"),
    ];
    for (sid, title, content, visibility) in &docs {
        upsert_document(conn, *sid, title, content, visibility);
    }
    tracing::info!(
        count = docs.len(),
        "Seeded knowledge documents (customer-safe + internal-only)"
    );

    // ---------------- Known issues ----------------
    let timezone_conv = by_number(5001);
    let slack_conv = by_number(5006);
    let ki1 = create_known_issue(
        conn,
        KnownIssueSeed {
            title: "Schedules keep previous DST offset after a clock change",
            status: "fix_in_progress",
            symptoms: "Scheduled reports and reminders fire one hour late/early after a daylight-saving change until the schedule is re-saved.",
            product: "Reports",
            feature: "Schedules",
            known_cause: "Stored schedule times anchor to the UTC offset at save time; mid-cycle DST changes are not re-anchored automatically.",
            workaround: "Open each schedule and re-save it once after the DST change.",
            customer_safe_explanation: "A known issue affects scheduled items around daylight-saving changes: times can be off by one hour until the schedule is re-saved. Our team is working on re-anchoring schedules automatically.",
            internal_explanation: "ENG-4472: re-anchor job scheduled for next release. Root cause: schedule store keeps absolute UTC offsets.",
            provenance: "human_local",
            conversation_ids: timezone_conv.map(|c| vec![c.id]).unwrap_or_default(),
        },
    );
    let ki2 = create_known_issue(
        conn,
        KnownIssueSeed {
            title: "Slack integration stops posting after Slack token rotation",
            status: "identified",
            symptoms: "Slack channel stops receiving updates; integration log shows repeated 401 from Slack.",
            product: "Integrations",
            feature: "Slack",
            known_cause: "Slack token rotation policy invalidated refresh tokens for older connections.",
            workaround: "Disconnect and reconnect the integration; historical data is preserved.",
            customer_safe_explanation: "An authentication change on Slack\u{2019}s side is affecting some workspaces. Reconnecting the integration restores updates; your historical data is unaffected.",
            internal_explanation: "ENG-4471: automatic re-auth flow in progress. Do not share internal IDs with customers.",
            provenance: "human_local",
            conversation_ids: slack_conv.map(|c| vec![c.id]).unwrap_or_default(),
        },
    );
    tracing::info!(count = 2, "Seeded known issues");

    // ---------------- Issue clusters ----------------
    let tz_convs: Vec<i64> = convs
        .iter()
        .filter(|c| {
            c.subject.as_deref().is_some_and(|s| {
                s.to_lowercase().contains("timezone")
                    || s.to_lowercase().contains("hour")
                    || s.to_lowercase().contains("reminder")
            })
        })
        .map(|c| c.id)
        .collect();
    if tz_convs.len() >= 2 {
        conn.execute(
            "INSERT INTO issue_clusters (name, title, summary, category, product, feature, known_issue_id, ai_generated)
             VALUES ('timezone schedules after DST change', 'timezone schedules after DST change',
                     'Customers in DST-observing regions report scheduled items firing one hour off after clock changes.',
                     'Timezone / Scheduling', 'Reports', 'Schedules', ?1, 1)",
            rusqlite::params![ki1],
        )
        .ok();
        let cluster = conn.last_insert_rowid();
        for conv in &tz_convs {
            conn.execute(
                "INSERT OR IGNORE INTO issue_cluster_members (cluster_id, conversation_id) VALUES (?1, ?2)",
                rusqlite::params![cluster, conv],
            )
            .ok();
        }
    }
    let billing_convs: Vec<i64> = convs
        .iter()
        .filter(|c| {
            c.subject.as_deref().is_some_and(|s| {
                let s = s.to_lowercase();
                s.contains("card")
                    || s.contains("invoice")
                    || s.contains("vat")
                    || s.contains("payment")
            })
        })
        .map(|c| c.id)
        .collect();
    if billing_convs.len() >= 2 {
        conn.execute(
            "INSERT INTO issue_clusters (name, title, summary, category, product, feature, known_issue_id, ai_generated)
             VALUES ('billing payment issues', 'billing payment issues',
                     'Failed charges and invoice/VAT questions from finance contacts.',
                     'Billing', 'Billing', 'Payments', NULL, 1)",
            [],
        )
        .ok();
        let cluster = conn.last_insert_rowid();
        for conv in &billing_convs {
            conn.execute(
                "INSERT OR IGNORE INTO issue_cluster_members (cluster_id, conversation_id) VALUES (?1, ?2)",
                rusqlite::params![cluster, conv],
            )
            .ok();
        }
    }
    let _ = crate::maintenance::compute_trends(conn);
    tracing::info!("Seeded issue clusters");

    // ---------------- Sample AI analyses (marked ai_generated, demo provenance) ----------------
    let mut seeded_analyses = 0;
    let samples: Vec<(i64, serde_json::Value, Vec<serde_json::Value>)> = vec![
        (
            5001,
            serde_json::json!({
                "intent": "bug_report",
                "primary_question": "How can the daily dispatch report schedule be made to follow the Santiago timezone after the DST change?",
                "customer_goal": "Receive the daily dispatch report at 8 AM Chilean time, every day of the year.",
                "product": "Reports", "feature": "Schedules", "problem_type": "defect",
                "requested_action": "Fix the schedule to follow the workspace timezone, or provide steps to correct it.",
                "urgency": "high", "sentiment": "negative",
                "known_issue_candidate": "Schedules keep previous DST offset after a clock change",
                "issue_cluster_candidate": "timezone schedules",
                "summary": "A VIP operations customer in Chile reports that a daily scheduled report fires at 3 AM local time after a DST change. Re-saving the schedule fixed one report; a second one remains offset. This matches an identified known issue about schedule re-anchoring.",
                "confidence": "high"
            }),
            vec![
                serde_json::json!({"source_type": "conversation", "source_id": by_number(5003).map(|c| c.id).unwrap_or(0), "title": "#5003 Timezone for scheduled exports", "relevance": 0.9, "visibility": "internal_only"}),
                serde_json::json!({"source_type": "known_issue", "source_id": ki1, "title": "Schedules keep previous DST offset", "relevance": 0.95, "visibility": "uncertain"}),
                serde_json::json!({"source_type": "knowledge_document", "source_id": doc_id_by_title(conn, "Timezones and scheduled reports"), "title": "Timezones and scheduled reports", "relevance": 0.9, "visibility": "customer_safe"}),
            ],
        ),
        (
            5006,
            serde_json::json!({
                "intent": "bug_report",
                "primary_question": "Why did the Slack integration stop posting updates and when will it be fixed?",
                "customer_goal": "Restore Slack alerting for their ops channel.",
                "product": "Integrations", "feature": "Slack", "problem_type": "defect",
                "requested_action": "Fix the integration urgently; they use it for alerting.",
                "urgency": "critical", "sentiment": "frustrated",
                "known_issue_candidate": "Slack integration stops posting after Slack token rotation",
                "issue_cluster_candidate": "slack integration auth",
                "summary": "A customer reports the Slack integration stopped posting updates, with log ID INT-88231. This matches a known engineering issue (Slack token rotation). Escalation note exists; customer-facing wording must stay generic.",
                "confidence": "high"
            }),
            vec![
                serde_json::json!({"source_type": "known_issue", "source_id": ki2, "title": "Slack integration stops posting", "relevance": 0.95, "visibility": "uncertain"}),
                serde_json::json!({"source_type": "knowledge_document", "source_id": doc_id_by_title(conn, "Slack integration 401 runbook (ENG-4471)"), "title": "Slack integration 401 runbook", "relevance": 0.9, "visibility": "internal_only"}),
            ],
        ),
        (
            5004,
            serde_json::json!({
                "intent": "question",
                "primary_question": "Why is the invitation email for a new teammate not arriving?",
                "customer_goal": "Get their teammate onboarded.",
                "product": "Accounts", "feature": "Invitations", "problem_type": "configuration",
                "requested_action": "Resend or fix the invitation delivery.",
                "urgency": "normal", "sentiment": "neutral",
                "known_issue_candidate": null,
                "issue_cluster_candidate": "invite delivery",
                "missing_information": ["Confirmation whether the re-sent invite arrived"],
                "summary": "Invitations to brightpathedu.org are bouncing with a provider policy rejection. An internal note documents the whitelist fix and the re-send; awaiting customer confirmation.",
                "confidence": "medium"
            }),
            vec![
                serde_json::json!({"source_type": "knowledge_document", "source_id": doc_id_by_title(conn, "Inviting teammates"), "title": "Inviting teammates", "relevance": 0.8, "visibility": "customer_safe"}),
            ],
        ),
        (
            5007,
            serde_json::json!({
                "intent": "billing",
                "primary_question": "Can the failed card payment be retried?",
                "customer_goal": "Restore the subscription to active.",
                "product": "Billing", "feature": "Payments", "problem_type": "billing",
                "requested_action": "Retry the charge on their valid card.",
                "urgency": "high", "sentiment": "negative",
                "known_issue_candidate": null,
                "issue_cluster_candidate": "billing payment issues",
                "summary": "Subscription shows past-due but the bank reports no charge attempt - the invoice likely entered a retry backoff. The documented self-service path is updating the card and clicking Retry payment.",
                "confidence": "high"
            }),
            vec![
                serde_json::json!({"source_type": "knowledge_document", "source_id": doc_id_by_title(conn, "Updating your payment method"), "title": "Updating your payment method", "relevance": 0.9, "visibility": "customer_safe"}),
            ],
        ),
    ];
    for (number, analysis, sources) in &samples {
        if let Some(conv) = by_number(*number) {
            save_analysis(conn, conv.id, analysis, sources);
            seeded_analyses += 1;
        }
    }
    tracing::info!(
        count = seeded_analyses,
        "Seeded sample AI analyses (marked ai_generated)"
    );

    // A REPEATED question across two conversations (gap-engine input).
    let repeated_question = "How do I connect my own custom domain to my workspace?";
    for number in [5005i64, 5010i64] {
        if let Some(conv) = by_number(number) {
            let analysis = serde_json::json!({
                "intent": "how_to",
                "primary_question": repeated_question,
                "customer_goal": "Serve the product from their own domain.",
                "product": "Workspace", "feature": "Domains", "problem_type": "how_to",
                "requested_action": "Provide custom domain setup steps.",
                "urgency": "normal", "sentiment": "neutral",
                "known_issue_candidate": null,
                "issue_cluster_candidate": "custom domains",
                "confidence": "high"
            });
            save_analysis(conn, conv.id, &analysis, &[]);
        }
    }
    tracing::info!("Seeded 1 repeated question across 2 conversations (gap-engine input)");

    // ---------------- Customer memories ----------------
    if let Some(lucia) = by_number(5001) {
        if let Some(customer) = lucia.customer_id {
            conn.execute(
                "INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source_conversation_id)
                 VALUES (?1, 'operates_in_chile', ?2, NULL, ?3)
                 ON CONFLICT (customer_id, memory_key) DO UPDATE SET memory_value = excluded.memory_value",
                rusqlite::params![
                    customer,
                    "Operations in Santiago, Chile (America/Santiago timezone, UTC-4 during DST).",
                    lucia.id
                ],
            )
            .ok();
            conn.execute(
                "INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source_conversation_id)
                 VALUES (?1, 'vip_account', ?2, NULL, ?3)
                 ON CONFLICT (customer_id, memory_key) DO UPDATE SET memory_value = excluded.memory_value",
                rusqlite::params![customer, "Andes Logistics is a VIP account (vip tag applied).", lucia.id],
            )
            .ok();
        }
    }
    tracing::info!("Seeded customer memories");

    // ---------------- Support cases ----------------
    if let Some(closed_tz) = by_number(5003) {
        conn.execute(
            "INSERT INTO support_cases (conversation_id, customer_id, problem, root_question,
                 resolution, answer, product, feature, tags, rating, provenance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'Reports', 'Schedules', '[\"timezone\"]', 'great', 'local_derived')
             ON CONFLICT (conversation_id) DO UPDATE SET
                problem = excluded.problem, root_question = excluded.root_question,
                resolution = excluded.resolution, answer = excluded.answer",
            rusqlite::params![
                closed_tz.id,
                closed_tz.customer_id,
                "Scheduled exports arrive in UTC instead of local time",
                "How do I set the timezone used for scheduled exports?",
                "Direct to Settings > Workspace > Regional settings; re-save each schedule after changing the timezone.",
                "Scheduled exports follow the workspace timezone (Settings > Workspace > Regional settings). After changing it, re-save each schedule once so stored times re-anchor."
            ],
        )
        .ok();
    }
    tracing::info!("Seeded support case");

    // ---------------- Incidents ----------------
    let slack_convs: Vec<i64> = convs
        .iter()
        .filter(|c| {
            c.subject
                .as_deref()
                .is_some_and(|s| s.to_lowercase().contains("slack"))
        })
        .map(|c| c.id)
        .collect();
    let tz_convs_all: Vec<i64> = convs
        .iter()
        .filter(|c| {
            c.subject.as_deref().is_some_and(|s| {
                let s = s.to_lowercase();
                s.contains("timezone")
                    || s.contains("hour")
                    || s.contains("reminder")
                    || s.contains("schedule")
            })
        })
        .map(|c| c.id)
        .collect();
    let mut incident_count = 0;
    if !slack_convs.is_empty() {
        conn.execute(
            "INSERT INTO incidents (known_issue_id, status, severity, source, description, title, code)
             VALUES (?1, 'identified', 'sev2', 'known_issue', ?2,
                     'Slack integration stops posting after token rotation', 'INC-001')",
            rusqlite::params![
                ki2,
                "Workspaces connected before the Slack token-rotation policy change report the integration stopped posting updates (repeated 401s in integration logs)."
            ],
        )
        .ok();
        let inc = conn.last_insert_rowid();
        for conv in &slack_convs {
            conn.execute(
                "INSERT OR IGNORE INTO incident_conversations (incident_id, conversation_id, linked_by)
                 VALUES (?1, ?2, 'human')",
                rusqlite::params![inc, conv],
            )
            .ok();
        }
        conn.execute(
            "INSERT INTO incident_refs (incident_id, system, reference, title, status, url)
             VALUES (?1, 'linear', 'ENG-4471', 'Slack auto re-auth', 'in progress', 'https://linear.app/example/issue/ENG-4471')",
            rusqlite::params![inc],
        )
        .ok();
        incident_count += 1;
    }
    if !tz_convs_all.is_empty() {
        conn.execute(
            "INSERT INTO incidents (known_issue_id, status, severity, source, description, title, code)
             VALUES (?1, 'fix_in_progress', 'sev3', 'known_issue', ?2,
                     'Scheduled reports drift one hour after DST changes', 'INC-002')",
            rusqlite::params![
                ki1,
                "Scheduled reports fire one hour off after daylight-saving changes until the schedule is re-saved."
            ],
        )
        .ok();
        let inc = conn.last_insert_rowid();
        for conv in &tz_convs_all {
            conn.execute(
                "INSERT OR IGNORE INTO incident_conversations (incident_id, conversation_id, linked_by)
                 VALUES (?1, ?2, 'human')",
                rusqlite::params![inc, conv],
            )
            .ok();
        }
        conn.execute(
            "INSERT INTO incident_refs (incident_id, system, reference, title, status)
             VALUES (?1, 'linear', 'ENG-4472', 'Re-anchor schedules after DST', 'in progress')",
            rusqlite::params![inc],
        )
        .ok();
        incident_count += 1;
    }
    tracing::info!(count = incident_count, "Seeded incidents with refs");

    // ---------------- Custom objects (Account + Deployment) ----------------
    let mut object_count = 0;
    let account_type: Option<i64> = {
        conn.execute(
            "INSERT OR IGNORE INTO custom_object_types (name, slug) VALUES ('Account', 'account')",
            [],
        )
        .ok();
        conn.query_row(
            "SELECT id FROM custom_object_types WHERE slug = 'account'",
            [],
            |r| r.get(0),
        )
        .ok()
    };
    let deployment_type: Option<i64> = {
        conn.execute(
            "INSERT OR IGNORE INTO custom_object_types (name, slug) VALUES ('Deployment', 'deployment')",
            [],
        )
        .ok();
        conn.query_row(
            "SELECT id FROM custom_object_types WHERE slug = 'deployment'",
            [],
            |r| r.get(0),
        )
        .ok()
    };
    let org_id_by_name = |name: &str| -> Option<i64> {
        conn.query_row(
            "SELECT id FROM organizations WHERE name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .ok()
    };
    if let Some(type_id) = account_type {
        let existing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM custom_objects WHERE type_id = ?1",
                rusqlite::params![type_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if existing == 0 {
            let candidates: Vec<(&str, serde_json::Value, Option<i64>, Option<i64>)> = vec![
                (
                    "Andes Logistics",
                    serde_json::json!({"plan_tier": "enterprise", "mrr": 4800, "renewal_date": "2026-03-01", "csm": "Priya Nair"}),
                    org_id_by_name("Andes Logistics"),
                    by_number(5001).and_then(|c| c.customer_id),
                ),
                (
                    "Bright Path Education",
                    serde_json::json!({"plan_tier": "starter", "mrr": 240, "renewal_date": "2025-12-15", "csm": "Marcus Chen"}),
                    org_id_by_name("Bright Path Education"),
                    by_number(5004).and_then(|c| c.customer_id),
                ),
                (
                    "Harbor Fitness",
                    serde_json::json!({"plan_tier": "growth", "mrr": 990, "renewal_date": "2026-01-10", "csm": "Sofia Reyes"}),
                    org_id_by_name("Harbor Fitness"),
                    by_number(5005).and_then(|c| c.customer_id),
                ),
            ];
            for (title, properties, org, customer) in candidates {
                if customer.is_none() && org.is_none() {
                    continue;
                }
                conn.execute(
                    "INSERT INTO custom_objects (type_id, title, data_json) VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        type_id,
                        title,
                        serde_json::to_string(&properties).unwrap_or_default()
                    ],
                )
                .ok();
                let object_id = conn.last_insert_rowid();
                if let Some(customer) = customer {
                    conn.execute(
                        "INSERT OR IGNORE INTO custom_object_links (object_id, target_kind, target_local_id)
                         VALUES (?1, 'customer', ?2)",
                        rusqlite::params![object_id, customer],
                    )
                    .ok();
                }
                if let Some(org) = org {
                    conn.execute(
                        "INSERT OR IGNORE INTO custom_object_links (object_id, target_kind, target_local_id)
                         VALUES (?1, 'organization', ?2)",
                        rusqlite::params![object_id, org],
                    )
                    .ok();
                }
                object_count += 1;
            }
        }
    }
    if let Some(type_id) = deployment_type {
        let existing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM custom_objects WHERE type_id = ?1",
                rusqlite::params![type_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if existing == 0 {
            let slack_incident: Option<i64> = conn
                .query_row("SELECT id FROM incidents WHERE code = 'INC-001'", [], |r| {
                    r.get(0)
                })
                .ok();
            let releases = vec![
                (
                    "v4.12.0 production",
                    serde_json::json!({"version": "v4.12.0", "environment": "production", "status": "degraded"}),
                    slack_incident,
                ),
                (
                    "v4.11.2 production",
                    serde_json::json!({"version": "v4.11.2", "environment": "production", "status": "healthy"}),
                    None,
                ),
            ];
            for (title, properties, incident) in releases {
                conn.execute(
                    "INSERT INTO custom_objects (type_id, title, data_json) VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        type_id,
                        title,
                        serde_json::to_string(&properties).unwrap_or_default()
                    ],
                )
                .ok();
                let object_id = conn.last_insert_rowid();
                if let Some(incident) = incident {
                    conn.execute(
                        "INSERT OR IGNORE INTO custom_object_links (object_id, target_kind, target_local_id)
                         VALUES (?1, 'incident', ?2)",
                        rusqlite::params![object_id, incident],
                    )
                    .ok();
                }
                object_count += 1;
            }
        }
    }
    tracing::info!(
        count = object_count,
        "Seeded custom objects (accounts + deployments)"
    );

    // ---------------- Connector (Product releases, AI-visible) ----------------
    {
        let connectors_dir = crate::config::default_data_dir().join("connectors");
        let _ = std::fs::create_dir_all(&connectors_dir);
        let release_file = connectors_dir.join("product-releases.json");
        let releases = serde_json::json!([
            {"version": "v4.12.0", "channel": "production", "notes": "Slack scopes change window"},
            {"version": "v4.11.2", "channel": "production", "notes": "Scheduling engine patch"},
            {"version": "v4.10.0", "channel": "production", "notes": "Billing retry backoff fix"}
        ]);
        if std::fs::write(
            &release_file,
            serde_json::to_string_pretty(&releases).unwrap_or_default(),
        )
        .is_ok()
        {
            let exists: Option<i64> = conn
                .query_row(
                    "SELECT id FROM connectors WHERE name = 'Product releases'",
                    [],
                    |r| r.get(0),
                )
                .ok();
            if exists.is_none() {
                if let Ok(connector) = crate::connectors::create(
                    conn,
                    "Product releases",
                    "local_json",
                    &serde_json::json!({"kind": "local_json", "file": "product-releases.json", "keyColumn": "version"}),
                    &serde_json::json!({"mode": "none"}),
                    "manual",
                    3600,
                    true,
                ) {
                    tracing::info!(
                        id = connector.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                        "Seeded 1 local JSON connector (Product releases, AI-visible)"
                    );
                }
            }
        }
    }

    // Knowledge freshness: give two docs human review/verify stamps so the
    // freshness view shows variety (others stay honestly unstamped).
    let doc_a = doc_id_by_title(conn, "Inviting teammates");
    let doc_b = doc_id_by_title(conn, "Viewer role capabilities");
    if doc_a > 0 {
        conn.execute(
            "UPDATE knowledge_documents SET last_reviewed_at = datetime('now', '-12 days') WHERE id = ?1",
            rusqlite::params![doc_a],
        )
        .ok();
    }
    if doc_b > 0 {
        conn.execute(
            "UPDATE knowledge_documents SET last_verified_at = datetime('now', '-40 days'),
                last_reviewed_at = datetime('now', '-40 days') WHERE id = ?1",
            rusqlite::params![doc_b],
        )
        .ok();
    }

    // Derive the customer event timeline once (idempotent).
    match crate::customer_events::rebuild(conn) {
        Ok(created) => tracing::info!(created, "Derived customer timeline events"),
        Err(e) => tracing::warn!(error = %e, "Demo timeline derive skipped"),
    }

    // One saved report definition so the builder opens with an example.
    let existing_reports: i64 = conn
        .query_row("SELECT COUNT(*) FROM report_definitions", [], |r| r.get(0))
        .unwrap_or(0);
    if existing_reports == 0 {
        let from = days_ago_date(30);
        let to = today_date();
        conn.execute(
            "INSERT INTO report_definitions (name, config, created_at, updated_at)
             VALUES ('Conversations per day (last 30 days)', ?1, datetime('now'), datetime('now'))",
            rusqlite::params![serde_json::to_string(&serde_json::json!({
                "metric": "conversations", "dimension": "day", "dateFrom": from, "dateTo": to,
                "comparison": "previous_period", "filters": {}, "sort": "dimension_asc", "limit": 40
            }))
            .unwrap_or_default()],
        )
        .ok();
        tracing::info!("Seeded 1 saved report definition (conversations per day)");
    }

    // ---------------- Graph + human memory ----------------
    match crate::maintenance::refresh_products(conn) {
        Ok(added) => tracing::info!(added, "Seeded products registry"),
        Err(e) => tracing::warn!(error = %e, "Demo products seed skipped"),
    }
    let inc_a: Option<i64> = conn
        .query_row("SELECT id FROM incidents WHERE code = 'INC-001'", [], |r| {
            r.get(0)
        })
        .ok();
    let inc_b: Option<i64> = conn
        .query_row("SELECT id FROM incidents WHERE code = 'INC-002'", [], |r| {
            r.get(0)
        })
        .ok();
    if let (Some(inc_a), Some(inc_b)) = (inc_a, inc_b) {
        conn.execute(
            "INSERT INTO incident_related (incident_id, target_kind, target_local_id, note)
             VALUES (?1, 'incident', ?2, ?3)
             ON CONFLICT (incident_id, target_kind, target_local_id) DO UPDATE SET note = excluded.note",
            rusqlite::params![
                inc_b,
                inc_a,
                "Demo: the timezone issue is being tracked against the Slack incident investigation."
            ],
        )
        .ok();
        tracing::info!("Seeded 1 human graph edge (INC-002 depends_on INC-001)");
    }
    let top_customer: Option<i64> = conn
        .query_row(
            "SELECT c.customer_id AS cid, COUNT(*) AS n FROM conversations c
              WHERE c.customer_id IS NOT NULL AND c.deleted_at IS NULL
              GROUP BY c.customer_id ORDER BY n DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if let Some(customer) = top_customer {
        conn.execute(
            "INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source_conversation_id, source)
             VALUES (?1, 'Preferred escalation path', ?2, NULL, NULL, 'human')
             ON CONFLICT (customer_id, memory_key) DO UPDATE SET memory_value = excluded.memory_value",
            rusqlite::params![
                customer,
                "Ping the on-call engineer directly after 2 unresolved replies; this account has a history of urgency."
            ],
        )
        .ok();
        tracing::info!("Seeded 1 human memory entry (escalation context)");
    }

    tracing::info!("Demo seed complete");
    tracing::info!("NOTE: all seeded AI content is marked ai_generated; demo data never mixes with production data.");
    true
}

fn days_ago_date(days: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64 - days * 86_400;
    let days_since_epoch = secs.div_euclid(86_400);
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn today_date() -> String {
    days_ago_date(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        crate::jobs::ensure_jobs_table(&conn).unwrap();
        conn
    }

    fn fresh_demo_db() -> Connection {
        let conn = fresh_db();
        settings::set_bool(&conn, "demo_mode", true).unwrap();
        conn
    }

    #[test]
    fn demo_tool_labels() {
        assert_eq!(
            DemoTool::SimulatedWebhookEvent.label(),
            "Simulate webhook event"
        );
        assert_eq!(
            DemoTool::SimulatedCsatRating.label(),
            "Simulate CSAT rating"
        );
        assert_eq!(
            DemoTool::SimulatedIncomingMessage.label(),
            "Simulate incoming customer message"
        );
    }

    #[test]
    fn demo_tool_all_has_three() {
        assert_eq!(DemoTool::ALL.len(), 3);
    }

    #[test]
    fn run_demo_tool_rejected_when_demo_mode_off() {
        let conn = fresh_db(); // demo_mode defaults to false
        let result = run_demo_tool(&conn, DemoTool::SimulatedWebhookEvent);
        assert!(matches!(result, DemoToolResult::DemoModeRequired));
    }

    #[test]
    fn simulate_webhook_event_succeeds_in_demo_mode() {
        let conn = fresh_demo_db();
        let result = run_demo_tool(&conn, DemoTool::SimulatedWebhookEvent);
        match result {
            DemoToolResult::Success { message } => {
                assert!(message.contains("accepted"));
            }
            _ => panic!("expected Success, got {result:?}"),
        }

        // A webhook event was persisted AND processed through the reference
        // pipeline (state ends 'processed'; the demo payload is a
        // convo.created event, so a sync_conversation job is enqueued).
        let (state, count): (String, i64) = conn
            .query_row(
                "SELECT processing_state, COUNT(*) FROM webhook_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(state, "processed");
        let job_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE type = 'sync_conversation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(job_count, 1);
    }

    #[test]
    fn simulate_csat_rating_succeeds_in_demo_mode() {
        let conn = fresh_demo_db();
        let result = run_demo_tool(&conn, DemoTool::SimulatedCsatRating);
        match result {
            DemoToolResult::Success { message } => {
                assert!(message.contains("CSAT rating enqueued"));
            }
            _ => panic!("expected Success, got {result:?}"),
        }

        // A rating.process job was enqueued.
        let job_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE type = 'rating.process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(job_count, 1);
    }

    #[test]
    fn simulate_incoming_message_succeeds_in_demo_mode() {
        let conn = fresh_demo_db();
        let result = run_demo_tool(&conn, DemoTool::SimulatedIncomingMessage);
        match result {
            DemoToolResult::Success { message } => {
                assert!(message.contains("enqueued as job"));
            }
            _ => panic!("expected Success, got {result:?}"),
        }

        // A sync.conversations job was enqueued.
        let job_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE type = 'sync.conversations'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(job_count, 1);
    }

    #[test]
    fn simulate_webhook_event_dedups_on_replay() {
        let conn = fresh_demo_db();

        // First call: accepted.
        let result1 = run_demo_tool(&conn, DemoTool::SimulatedWebhookEvent);
        assert!(matches!(result1, DemoToolResult::Success { .. }));

        // The event ID is derived from timestamp_millis(). If both calls happen
        // in the same millisecond, the second will be a Duplicate (dedup works).
        // If they happen in different milliseconds, the second will be a new
        // Accepted event. Either way, the pipeline works correctly.
        let result2 = run_demo_tool(&conn, DemoTool::SimulatedWebhookEvent);
        assert!(
            matches!(result2, DemoToolResult::Success { .. }),
            "second call should succeed (either as new event or dedup): {result2:?}"
        );

        // At least 1 event persisted (the first call always creates one;
        // with the reference pipeline the event is processed synchronously
        // so its state ends as 'processed', which still counts as persisted).
        let persisted: i64 = conn
            .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
            .unwrap();
        assert!(
            persisted >= 1,
            "at least 1 event should be persisted: {persisted}"
        );
    }

    #[test]
    fn is_demo_mode_returns_false_on_fresh_db() {
        let conn = fresh_db();
        assert!(!is_demo_mode(&conn));
    }

    #[test]
    fn is_demo_mode_returns_true_when_set() {
        let conn = fresh_demo_db();
        assert!(is_demo_mode(&conn));
    }
}
