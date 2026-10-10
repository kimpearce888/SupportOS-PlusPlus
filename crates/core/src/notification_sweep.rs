//! NotificationSweep — the single producer of Notification Center rows.
//! Faithful port of src/server/notifications/notificationSweep.ts.
//!
//! Design decisions (ported verbatim):
//! - One funnel, like the sync engine's ingestConversation: every
//!   notification type is derived from an observable local fact
//!   (activity events, SLA alerts, jobs, sync state, outreach recipients,
//!   known issues/clusters, ai_runs, ratings) — never from free-form
//!   producers, never from guesses.
//! - Idempotence by dedup keys: event-derived notifications reuse the
//!   event's own dedup key; state-derived ones (SLA, spikes, sync
//!   failures) include the day so at most one notification per subject
//!   per day. Re-running the sweep, re-syncing, or crashing mid-sweep can
//!   never duplicate.
//! - First run is SILENT by design: the cursor initializes to "now",
//!   because history did not notify anyone (same honesty rule as
//!   conversation_events pre-sync history).
//! - The sweep reads the event log the SYNC writes (thread-derived
//!   customer_message/internal_note events from `upsert_thread` and
//!   assignment_changed observation diffs from `upsert_conversation`),
//!   plus the synced state tables (SLA alerts, jobs, outreach, known
//!   issues, clusters, ai_runs, ratings) — never a table nothing
//!   populates.
//! - All timestamp comparisons go through `before_or_equal`, which
//!   accepts both the `datetime('now')` space format and ISO-Z strings —
//!   never bare string compares (the v1.6.0 audit bug class).
//! - Preferences are checked BEFORE insert (inside
//!   [`crate::notifications::record_notification`]): a disabled type
//!   produces no row at all.
//! - The event cursor advances even if a step threw: each step is
//!   individually idempotent, so a partially failed sweep never
//!   re-notifies.

use rusqlite::{params, Connection};

use crate::catalog::NotificationType;
use crate::error::Result;
use crate::mentions::{build_mention_directory, parse_mentions};
use crate::notifications::{record_notification, NotificationInput};

/// The settings key for the event cursor (max `activity_events.id`
/// processed). Reference: `notif_event_cursor`.
pub const CURSOR_KEY: &str = "notif_event_cursor";

/// The settings key for the last sweep's start stamp. Reference:
/// `notif_sweep_at`.
pub const SWEEP_AT_KEY: &str = "notif_sweep_at";

/// The result of one sweep pass — how many notifications were created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepOutcome {
    pub created: u32,
}

/// Check whether the first sync has settled — the reference's v1.8.0 fix:
/// the sweep cursor must not init while the mirror is still populating
/// (NEW/INITIALIZING/BACKFILLING). CATCHING_UP, LIVE and ERROR all mean
/// the mirror settled — the sweep does not require the sync to have
/// succeeded, only that it has finished.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the settings read fails.
pub fn first_sync_settled(conn: &Connection) -> Result<bool> {
    let state = crate::sync_engine::get_state(conn);
    Ok(!matches!(
        state.as_str(),
        "NEW" | "INITIALIZING" | "BACKFILLING"
    ))
}

/// One sweep pass. Returns how many notifications were created. Safe to
/// re-run at any time; safe when tables are empty.
///
/// # Errors
///
/// Returns `Error::Sqlite` if a database step fails — the cursor still
/// advances past the processed events (the reference's `finally`), so a
/// partially failed sweep never re-notifies.
/// A sweep source row: (id, kind, status, error, payload).
type SweepRow = (i64, String, String, Option<String>, Option<String>);

pub fn sweep(conn: &Connection, bus: Option<&crate::http::EventBus>) -> Result<SweepOutcome> {
    let now = now_stamp();
    let cursor_raw = crate::settings::get_i64(conn, CURSOR_KEY, -1)?;
    let sweep_at = crate::settings::get_string(conn, SWEEP_AT_KEY)?;

    let (cursor, sweep_at) = match (cursor_raw >= 0, sweep_at) {
        (true, Some(at)) => (cursor_raw, at),
        _ => {
            // v1.8.0 fix: do NOT initialize the cursor while the first sync
            // is still running — the boot sweep used to initialize it BEFORE
            // the initial sync populated the event log, so the entire
            // first-sync history arrived "after the cursor" and notified.
            if !first_sync_settled(conn)? {
                return Ok(SweepOutcome::default());
            }
            // First run after the mirror settled: initialize silently
            // (history did not notify anyone).
            crate::settings::set_i64(conn, CURSOR_KEY, max_event_id(conn))?;
            crate::settings::set_string(conn, SWEEP_AT_KEY, &now)?;
            return Ok(SweepOutcome::default());
        }
    };

    let result = run_sub_sweeps(conn, bus, cursor, &sweep_at);

    // The cursor advances even if a step threw: each step is individually
    // idempotent, so a partially failed sweep never re-notifies.
    crate::settings::set_i64(conn, CURSOR_KEY, max_event_id(conn))?;
    crate::settings::set_string(conn, SWEEP_AT_KEY, &now)?;

    result.map(|created| SweepOutcome { created })
}

/// All ten sub-sweeps in the reference's order.
fn run_sub_sweeps(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    cursor: i64,
    sweep_at: &str,
) -> Result<u32> {
    let mut created = 0u32;
    created += sweep_conversation_events(conn, bus, cursor)?;
    created += sweep_sla(conn, bus)?;
    created += sweep_automation_approvals(conn, bus)?;
    created += sweep_failed_jobs(conn, bus, sweep_at)?;
    created += sweep_sync_state(conn, bus)?;
    created += sweep_campaign_replies(conn, bus, sweep_at)?;
    created += sweep_known_issues(conn, bus, sweep_at)?;
    created += sweep_issue_spikes(conn, bus, sweep_at)?;
    created += sweep_ai_escalations(conn, bus, sweep_at)?;
    created += sweep_ratings(conn, bus, sweep_at)?;
    Ok(created)
}

// ---------------- 1. conversation events ----------------

/// One row of the event scan.
struct EventRow {
    conversation_id: i64,
    thread_local_id: Option<i64>,
    event_type: String,
    actor_local_id: Option<i64>,
    metadata: Option<String>,
    dedup_key: String,
}

/// The conversation facts a notification needs (local-first resolution:
/// legacy event rows keyed by remote id still resolve).
struct ConversationFacts {
    id: i64,
    number: Option<i64>,
    subject: Option<String>,
    assignee_local_id: Option<i64>,
    customer_local_id: Option<i64>,
}

fn load_conversation(conn: &Connection, id_or_remote: i64) -> Result<Option<ConversationFacts>> {
    let row = conn
        .query_row(
            "SELECT id, number, subject, assignee_id, customer_id
             FROM conversations
             WHERE id = ?1 OR remote_id = ?1
             ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END
             LIMIT 1",
            params![id_or_remote],
            |r| {
                Ok(ConversationFacts {
                    id: r.get(0)?,
                    number: r.get(1)?,
                    subject: r.get(2)?,
                    assignee_local_id: r.get(3)?,
                    customer_local_id: r.get(4)?,
                })
            },
        )
        .ok();
    Ok(row)
}

fn sweep_conversation_events(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    after_event_id: i64,
) -> Result<u32> {
    let events: Vec<EventRow> = {
        let mut stmt = conn.prepare(
            "SELECT e.conversation_id, e.thread_local_id, e.event_type,
                    e.actor_id, e.metadata, e.dedup_key
             FROM activity_events e
             WHERE e.id > ?1
               AND e.event_type IN ('customer_message', 'internal_note', 'assignment_changed')
             ORDER BY e.id LIMIT 500",
        )?;
        let rows = stmt
            .query_map(params![after_event_id], |r| {
                Ok(EventRow {
                    conversation_id: r.get(0)?,
                    thread_local_id: r.get(1)?,
                    event_type: r.get(2)?,
                    actor_local_id: r.get(3)?,
                    metadata: r.get(4)?,
                    dedup_key: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if events.is_empty() {
        return Ok(0);
    }
    let mut created = 0u32;
    for e in &events {
        let Some(conv) = load_conversation(conn, e.conversation_id)? else {
            continue; // merged/deleted
        };
        let conv_id = conv.id;
        let number = conv.number;
        match e.event_type.as_str() {
            "customer_message" => {
                let excerpt = thread_excerpt(conn, e.thread_local_id)?;
                let subject_suffix = conv
                    .subject
                    .as_deref()
                    .map(|s| format!(" — {}", trim(s, 60)))
                    .unwrap_or_default();
                let r = record_notification(
                    conn,
                    bus,
                    &NotificationInput {
                        notification_type: NotificationType::CustomerReplied,
                        title: format!(
                            "Customer replied on #{}{}",
                            number.unwrap_or(conv_id),
                            subject_suffix
                        ),
                        body: excerpt,
                        target_user_local_id: conv.assignee_local_id,
                        conversation_id: Some(conv_id),
                        conversation_number: number,
                        customer_local_id: conv.customer_local_id,
                        dedup_key: format!("n:evt:customer_replied:{}", e.dedup_key),
                        ..Default::default()
                    },
                )?;
                if r.is_some() {
                    created += 1;
                }
            }
            "assignment_changed" => {
                let meta = safe_json_object(e.metadata.as_deref());
                let next = meta.get("next").and_then(|v| v.as_i64());
                if let Some(next) = next {
                    if Some(next) != e.actor_local_id {
                        let name = user_display(conn, Some(next));
                        let r = record_notification(
                            conn,
                            bus,
                            &NotificationInput {
                                notification_type: NotificationType::TicketAssigned,
                                title: format!("#{} assigned to {name}", number.unwrap_or(conv_id)),
                                body: conv.subject.as_deref().map(|s| trim(s, 120)),
                                target_user_local_id: Some(next),
                                actor_user_local_id: e.actor_local_id,
                                conversation_id: Some(conv_id),
                                conversation_number: number,
                                dedup_key: format!("n:evt:ticket_assigned:{}", e.dedup_key),
                                ..Default::default()
                            },
                        )?;
                        if r.is_some() {
                            created += 1;
                        }
                    }
                }
            }
            "internal_note" => {
                // Mentions inside internal notes (plan Phase 13).
                let Some(body) = thread_body(conn, e.thread_local_id)? else {
                    continue;
                };
                let directory = build_mention_directory(conn)?;
                let mentions = parse_mentions(&body, &directory)?;
                let author = e.actor_local_id;
                for m in mentions {
                    let Some(user) = m.user_local_id else {
                        continue;
                    };
                    if Some(user) == author {
                        continue;
                    }
                    let r = record_notification(
                        conn,
                        bus,
                        &NotificationInput {
                            notification_type: NotificationType::Mentioned,
                            title: format!(
                                "{} mentioned you on #{}",
                                user_display(conn, author),
                                number.unwrap_or(conv_id)
                            ),
                            body: Some(trim(&body, 200)),
                            target_user_local_id: Some(user),
                            actor_user_local_id: author,
                            conversation_id: Some(conv_id),
                            conversation_number: number,
                            dedup_key: format!("n:mention:{}:{user}", e.dedup_key),
                            ..Default::default()
                        },
                    )?;
                    if r.is_some() {
                        created += 1;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(created)
}

// ---------------- 2. SLA risk / breach ----------------

fn sweep_sla(conn: &Connection, bus: Option<&crate::http::EventBus>) -> Result<u32> {
    let mut created = 0u32;
    let alerts = crate::sla::sla_alerts(conn)?;
    let day = day_stamp();
    for a in &alerts.alerts {
        let breached = a.state == crate::sla::SlaAlertState::Breached;
        let notif_type = if breached {
            NotificationType::SlaBreach
        } else {
            NotificationType::SlaRisk
        };
        let title = if breached {
            format!(
                "SLA breached on #{} ({}) — {} min over target",
                a.number, a.mailbox_name, a.overdue_business_min
            )
        } else {
            format!(
                "SLA at risk on #{} ({}) — {}/{} business min",
                a.number, a.mailbox_name, a.waited_business_min, a.target_min
            )
        };
        let r = record_notification(
            conn,
            bus,
            &NotificationInput {
                notification_type: notif_type,
                severity: Some(if breached { "critical" } else { "warning" }),
                title,
                body: a.subject.as_deref().map(|s| trim(s, 120)),
                target_user_local_id: a.assignee_local_id,
                conversation_id: Some(a.conversation_id),
                conversation_number: Some(a.number),
                dedup_key: format!("n:sla:{}:{}:{day}", a.state.as_str(), a.conversation_id),
                ..Default::default()
            },
        )?;
        if r.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

// ---------------- 3. automation approvals ----------------

fn sweep_automation_approvals(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
) -> Result<u32> {
    let rows: Vec<(i64, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, payload FROM jobs
             WHERE type = 'automation_action_awaiting_approval'
               AND status IN ('queued', 'parked')
             ORDER BY id LIMIT 100",
        )?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    let mut created = 0u32;
    for (job_id, payload) in rows {
        let payload = safe_json_object(payload.as_deref());
        let conversation_id = payload.get("conversationId").and_then(|v| v.as_i64());
        let conv = conversation_id.and_then(|id| load_conversation(conn, id).ok().flatten());
        let action = payload
            .get("action")
            .and_then(|a| a.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("action")
            .to_string();
        let r = record_notification(
            conn,
            bus,
            &NotificationInput {
                notification_type: NotificationType::AutomationApproval,
                severity: Some("warning"),
                title: format!(
                    "Automation approval required: {action}{}",
                    conv.as_ref()
                        .and_then(|c| c.number)
                        .map(|n| format!(" on #{n}"))
                        .unwrap_or_default()
                ),
                body: conv
                    .as_ref()
                    .and_then(|c| c.subject.as_deref())
                    .map(|s| trim(s, 120)),
                conversation_id: conv.as_ref().map(|c| c.id),
                conversation_number: conv.as_ref().and_then(|c| c.number),
                job_id: Some(job_id),
                dedup_key: format!("n:approval:{job_id}"),
                ..Default::default()
            },
        )?;
        if r.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

// ---------------- 4. failed jobs ----------------

fn sweep_failed_jobs(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    since_stamp: &str,
) -> Result<u32> {
    let rows: Vec<SweepRow> = {
        let mut stmt = conn.prepare(
            "SELECT id, queue, type, error, completed_at FROM jobs
             WHERE status = 'failed' ORDER BY id DESC LIMIT 200",
        )?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    let mut created = 0u32;
    for (job_id, queue, job_type, error, completed_at) in rows {
        let Some(completed) = completed_at else {
            continue;
        };
        if before_or_equal(&completed, since_stamp) {
            continue;
        }
        let r = record_notification(
            conn,
            bus,
            &NotificationInput {
                notification_type: NotificationType::JobFailure,
                severity: Some("warning"),
                title: format!("Background job failed ({queue}/{job_type})"),
                body: error.as_deref().map(|e| trim(e, 200)),
                job_id: Some(job_id),
                dedup_key: format!("n:jobfail:{job_id}"),
                ..Default::default()
            },
        )?;
        if r.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

// ---------------- 5. sync state ----------------

fn sweep_sync_state(conn: &Connection, bus: Option<&crate::http::EventBus>) -> Result<u32> {
    let state = crate::sync_engine::get_state(conn);
    if state != "ERROR" {
        return Ok(0);
    }
    let r = record_notification(
        conn,
        bus,
        &NotificationInput {
            notification_type: NotificationType::SyncFailure,
            severity: Some("critical"),
            title: "Help Scout sync is in ERROR state".into(),
            body: Some("Open Sync Health to see the last errors and restart the sync.".into()),
            dedup_key: format!("n:syncfail:{}", day_stamp()),
            ..Default::default()
        },
    )?;
    Ok(u32::from(r.is_some()))
}

// ---------------- 6. campaign replies ----------------

fn sweep_campaign_replies(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    since_stamp: &str,
) -> Result<u32> {
    /// Row tuple: (i64, Option<i64>, Option<i64>, Option<String>, Option<String>, Option<String>).
    type SweepRows0 = (
        i64,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<SweepRows0> = {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.campaign_id, r.customer_local_id, r.replied_at,
                    c.name, c.subject
             FROM outreach_recipients r JOIN outreach_campaigns c ON c.id = r.campaign_id
             WHERE r.replied_at IS NOT NULL ORDER BY r.id DESC LIMIT 200",
        )?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    let mut created = 0u32;
    for (id, campaign_id, customer_local_id, replied_at, name, subject) in rows {
        let Some(replied_at) = replied_at else {
            continue;
        };
        if before_or_equal(&replied_at, since_stamp) {
            continue;
        }
        let customer = customer_display(conn, customer_local_id);
        let campaign_name = name.as_deref().unwrap_or("campaign");
        let r = record_notification(
            conn,
            bus,
            &NotificationInput {
                notification_type: NotificationType::CampaignReply,
                title: format!(
                    "Campaign reply: {} answered \"{}\"",
                    customer.as_deref().unwrap_or("a customer"),
                    trim(campaign_name, 50)
                ),
                body: subject.as_deref().map(|s| trim(s, 120)),
                customer_local_id,
                campaign_id,
                dedup_key: format!("n:campreply:{id}"),
                ..Default::default()
            },
        )?;
        if r.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

// ---------------- 7. known issues ----------------

fn sweep_known_issues(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    since_stamp: &str,
) -> Result<u32> {
    /// Row tuple: (i64 Option<String> Option<String> Option<String> Option<String>).
    type SweepRows1 = (
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<SweepRows1> = {
        let mut stmt = conn.prepare(
            "SELECT id, title, name, status, created_at FROM known_issues
             ORDER BY id DESC LIMIT 100",
        )?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    let mut created = 0u32;
    for (id, title, name, status, created_at) in rows {
        let at = created_at.unwrap_or_default();
        if before_or_equal(&at, since_stamp) {
            continue;
        }
        let display = title.or(name).unwrap_or_default();
        let r = record_notification(
            conn,
            bus,
            &NotificationInput {
                notification_type: NotificationType::KnownIssueDetected,
                title: format!("Known issue tracked: {}", trim(&display, 80)),
                body: Some(format!(
                    "Status: {}",
                    status.as_deref().unwrap_or("investigating")
                )),
                issue_id: Some(id),
                dedup_key: format!("n:knownissue:{id}"),
                ..Default::default()
            },
        )?;
        if r.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

// ---------------- 8. issue spikes ----------------

fn sweep_issue_spikes(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    since_stamp: &str,
) -> Result<u32> {
    /// Row tuple: (i64 Option<String> Option<String> Option<i64> Option<String>).
    type SweepRows2 = (
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    );
    let rows: Vec<SweepRows2> = {
        let mut stmt = conn.prepare(
            "SELECT id, title, name, conversation_count, updated_at FROM issue_clusters
             WHERE trend = 'rising' ORDER BY id DESC LIMIT 100",
        )?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    let mut created = 0u32;
    let day = day_stamp();
    for (id, title, name, conversation_count, updated_at) in rows {
        let at = updated_at.unwrap_or_default();
        if before_or_equal(&at, since_stamp) {
            continue;
        }
        let display = title.or(name).unwrap_or_default();
        let r = record_notification(
            conn,
            bus,
            &NotificationInput {
                notification_type: NotificationType::IssueSpike,
                severity: Some("warning"),
                title: format!(
                    "Issue spike: {} ({} conversations)",
                    trim(&display, 80),
                    conversation_count.unwrap_or(0)
                ),
                issue_id: Some(id),
                dedup_key: format!("n:spike:{id}:{day}"),
                ..Default::default()
            },
        )?;
        if r.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

// ---------------- 9. AI escalation ----------------

fn sweep_ai_escalations(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    since_stamp: &str,
) -> Result<u32> {
    /// Row tuple: (i64 i64 Option<String> Option<String> Option<i64> Option<String> Option<String>).
    type SweepRows3 = (
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<SweepRows3> = {
        let mut stmt = conn.prepare(
            "SELECT a.id, a.conversation_id, a.response_json, a.created_at,
                    c.number, c.subject, c.status
             FROM ai_runs a JOIN conversations c ON c.id = a.conversation_id
             WHERE a.type = 'ticket_analysis' AND a.status = 'completed'
               AND a.conversation_id IS NOT NULL
             ORDER BY a.id DESC LIMIT 200",
        )?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    let mut created = 0u32;
    for (run_id, conversation_id, output, created_at, number, subject, status) in rows {
        let at = created_at.unwrap_or_default();
        if before_or_equal(&at, since_stamp) {
            continue;
        }
        if status.as_deref() == Some("closed") {
            continue; // escalation on a closed ticket is noise
        }
        let analysis = safe_json_object(output.as_deref());
        let urgency = analysis.get("urgency").and_then(|v| v.as_str());
        let sentiment = analysis.get("sentiment").and_then(|v| v.as_str());
        let confidence = analysis
            .get("confidence")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let escalate = (matches!(urgency, Some("high") | Some("critical"))
            || sentiment == Some("frustrated"))
            && matches!(confidence, "medium" | "high");
        if !escalate {
            continue;
        }
        let urgency_suffix = match urgency {
            Some("critical") => " — urgency: critical",
            Some("high") => " — urgency: high",
            _ => " — customer frustrated",
        };
        let body = [
            subject.as_deref().map(|s| trim(s, 100)),
            Some(format!("AI confidence: {confidence}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let r = record_notification(
            conn,
            bus,
            &NotificationInput {
                notification_type: NotificationType::AiEscalation,
                severity: Some("warning"),
                title: format!(
                    "AI escalation on #{}{urgency_suffix}",
                    number.unwrap_or(conversation_id)
                ),
                body: if body.is_empty() { None } else { Some(body) },
                conversation_id: Some(conversation_id),
                conversation_number: number,
                dedup_key: format!("n:aiescal:{run_id}"),
                ..Default::default()
            },
        )?;
        if r.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

// ---------------- 10. important customer events (ratings) ----------------

fn sweep_ratings(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    since_stamp: &str,
) -> Result<u32> {
    /// Rating sweep row: (id, remote_id, customer_local_id, comments, conv).
    type RatingRow = (i64, Option<i64>, Option<i64>, Option<String>, Option<i64>);
    let rows: Vec<RatingRow> = {
        let mut stmt = conn.prepare(
            "SELECT id, remote_id, customer_local_id, comments, conversation_id
             FROM ratings WHERE rating = 'not-good' ORDER BY id DESC LIMIT 100",
        )?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    let mut created = 0u32;
    for (id, remote_id, customer_local_id, comments, conversation_id) in rows {
        // The remote_created_at stamp decides freshness.
        let at: Option<String> = conn
            .query_row(
                "SELECT remote_created_at FROM ratings WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let at = at.unwrap_or_default();
        if before_or_equal(&at, since_stamp) {
            continue;
        }
        let customer = customer_display(conn, customer_local_id);
        let r = record_notification(
            conn,
            bus,
            &NotificationInput {
                notification_type: NotificationType::CustomerEvent,
                title: format!(
                    "Not-good rating received{}",
                    customer
                        .as_deref()
                        .map(|c| format!(" from {c}"))
                        .unwrap_or_default()
                ),
                body: comments.as_deref().map(|c| trim(c, 200)),
                customer_local_id,
                conversation_id,
                dedup_key: format!(
                    "n:rating:{}",
                    remote_id.map_or_else(|| format!("id{id}"), |r| r.to_string())
                ),
                ..Default::default()
            },
        )?;
        if r.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

// ---------------- helpers ----------------

/// The current max `activity_events.id` (0 when the log is empty).
fn max_event_id(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(MAX(id), 0) AS m FROM activity_events",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Space-format UTC stamp (matches `datetime('now')` storage) — the
/// reference `nowStamp()`.
fn now_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// YYYYMMDD UTC — the reference `dayStamp()`.
fn day_stamp() -> String {
    chrono::Utc::now().format("%Y%m%d").to_string()
}

/// The stored body of a thread (plain lookup by local id).
fn thread_body(conn: &Connection, thread_local_id: Option<i64>) -> Result<Option<String>> {
    let Some(id) = thread_local_id else {
        return Ok(None);
    };
    let body: Option<String> = conn
        .query_row(
            "SELECT body_text FROM conversation_threads WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    Ok(body)
}

/// The thread body trimmed to 160 chars — the reference `threadExcerpt`.
fn thread_excerpt(conn: &Connection, thread_local_id: Option<i64>) -> Result<Option<String>> {
    Ok(thread_body(conn, thread_local_id)?.map(|b| trim(&b, 160)))
}

/// The display name of a user — the reference `userDisplay`.
fn user_display(conn: &Connection, user_local_id: Option<i64>) -> String {
    let Some(id) = user_local_id else {
        return "Someone".into();
    };
    let row: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT first_name, last_name, email FROM users WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    match row {
        None => format!("user #{id}"),
        Some((first, last, email)) => {
            let name = [first, last]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            if !name.is_empty() {
                name
            } else {
                email.unwrap_or_else(|| format!("user #{id}"))
            }
        }
    }
}

/// The display name of a customer — the reference `customerDisplay`.
fn customer_display(conn: &Connection, customer_local_id: Option<i64>) -> Option<String> {
    let id = customer_local_id?;
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT first_name, last_name FROM customers WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    row.and_then(|(first, last)| {
        let name = [first, last]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        (!name.is_empty()).then_some(name)
    })
}

/// Parse a stored JSON object (NULL/garbage → empty object) — the
/// reference `safeJson`.
fn safe_json_object(v: Option<&str>) -> serde_json::Value {
    v.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null)
        .as_object()
        .cloned()
        .map(serde_json::Value::Object)
        .unwrap_or(serde_json::json!({}))
}

/// True when a stamp is at or before the reference (format-agnostic) —
/// the reference `beforeOrEqual`. Unparseable stamps count as already
/// seen (no spam).
fn before_or_equal(stamp: &str, reference: &str) -> bool {
    match (parse_stamp(stamp), parse_stamp(reference)) {
        (Some(a), Some(b)) => a <= b,
        _ => true,
    }
}

/// Parse either the space format ("2026-01-01 10:00:00") or ISO-Z — the
/// reference `Date.parse` shim.
fn parse_stamp(s: &str) -> Option<i64> {
    let normalized = if s.contains('T') {
        s.to_string()
    } else {
        format!("{}Z", s.replace(' ', "T"))
    };
    chrono::DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// The reference `trim(s, max)`: trim, then cap to `max` chars with an
/// ellipsis.
fn trim(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() > max {
        let cut: String = t.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{record_event, ActivityEvent};
    use crate::notifications::me_user_local_id;
    use crate::notifications::unread_count;
    use rusqlite::params;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        // The canonical boot chain — the sweep reads the REAL schema
        // (activity_events with metadata/source/thread_local_id, the
        // reference-shaped notifications table), never a partial one.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        // DB-06: the notification FKs — `insert_conversation` stamps
        // customer 2001 (the notifications carry customer_local_id).
        // Users stay test-owned: the mention/assignee fixtures seed their
        // own (with mention names).
        conn.execute_batch(
            "INSERT INTO customers (id, remote_id, first_name)
             VALUES (2001, 92001, 'Ada');",
        )
        .unwrap();
        conn
    }

    fn mark_sync_settled(conn: &Connection) {
        crate::sync_engine::set_state(conn, "LIVE");
    }

    fn insert_conversation(conn: &Connection, remote_id: i64, assignee_local: Option<i64>) -> i64 {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id, assignee_id)
             VALUES (?1, ?1, 'Printer on fire', 'active', 101, 2001, ?2)",
            params![remote_id, assignee_local],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_thread(
        conn: &Connection,
        conversation_local: i64,
        kind: &str,
        body: &str,
        actor_type: &str,
        actor_id: Option<i64>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO conversation_threads
                 (conversation_id, type, state, body_text, from_type,
                  created_by_user_id, created_by_customer_id,
                  created_by_system_user_id, created_at)
             VALUES (?1, ?2, 'published', ?3, ?4,
                     CASE WHEN ?4 = 'user' THEN ?5 END,
                     CASE WHEN ?4 = 'customer' THEN ?5 END,
                     CASE WHEN ?4 IN ('system', 'system_user') THEN ?5 END,
                     datetime('now'))",
            params![conversation_local, kind, body, actor_type, actor_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Record an event the way the SYNC does (thread-derived or an
    /// assignment observation diff).
    #[allow(clippy::too_many_arguments)]
    fn insert_event(
        conn: &Connection,
        conversation_local: i64,
        thread_local_id: Option<i64>,
        event_type: &str,
        actor_type: &str,
        actor_local_id: Option<i64>,
        metadata: &str,
        dedup: &str,
    ) {
        record_event(
            conn,
            &ActivityEvent {
                id: None,
                conversation_id: conversation_local,
                event_type: event_type.into(),
                actor_type: actor_type.into(),
                actor_id: actor_local_id,
                occurred_at: "2026-01-01T10:00:00Z".into(),
                dedup_key: dedup.into(),
            },
        )
        .unwrap();
        // The breadth columns (thread_local_id/metadata) are not part of
        // ActivityEvent — stamp them like upsert_thread/upsert_conversation.
        conn.execute(
            "UPDATE activity_events SET thread_local_id = ?1, metadata = ?2 WHERE dedup_key = ?3",
            params![thread_local_id, metadata, dedup],
        )
        .unwrap();
    }

    fn count_type(conn: &Connection, notif_type: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM notifications WHERE type = ?1",
            params![notif_type],
            |r| r.get(0),
        )
        .unwrap()
    }

    // ---- cursor lifecycle ---------------------------------------------------

    #[test]
    fn sweep_is_silent_while_the_first_sync_is_populating() {
        let conn = fresh_db();
        // No sync_state setting → reference default 'NEW' → not settled.
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0);
        for state in ["NEW", "INITIALIZING", "BACKFILLING"] {
            crate::sync_engine::set_state(&conn, state);
            let out = sweep(&conn, None).unwrap();
            assert_eq!(out.created, 0, "state {state}");
        }
        // The cursor must NOT be initialized.
        let cursor = crate::settings::get_i64(&conn, CURSOR_KEY, -1).unwrap();
        assert_eq!(cursor, -1, "cursor stays uninitialized");
    }

    #[test]
    fn first_sweep_after_settle_initializes_the_cursor_silently() {
        let conn = fresh_db();
        let conv = insert_conversation(&conn, 1001, None);
        let t = insert_thread(&conn, conv, "customer", "hello", "customer", None);
        insert_event(
            &conn,
            conv,
            Some(t),
            "customer_message",
            "customer",
            None,
            "{}",
            "thread:9001",
        );
        mark_sync_settled(&conn);

        // History did not notify anyone — the first sweep only initializes.
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0);
        let cursor = crate::settings::get_i64(&conn, CURSOR_KEY, -1).unwrap();
        assert!(cursor > 0, "cursor initialized to the max event id");
        assert!(crate::settings::get_string(&conn, SWEEP_AT_KEY)
            .unwrap()
            .is_some());
        assert_eq!(count_type(&conn, "customer_replied"), 0);
    }

    #[test]
    fn settled_states_include_error_and_catching_up() {
        for state in ["CATCHING_UP", "LIVE", "ERROR"] {
            let conn = fresh_db();
            crate::sync_engine::set_state(&conn, state);
            assert!(first_sync_settled(&conn).unwrap(), "state {state}");
        }
    }

    // ---- 1. conversation events --------------------------------------------

    #[test]
    fn customer_message_event_notifies_the_assignee() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO users (id, remote_id, first_name, last_name, user_type)
             VALUES (42, 42, 'Alex', 'Rivera', 'user')",
            [],
        )
        .unwrap();
        let conv = insert_conversation(&conn, 1001, Some(42));
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap(); // init cursor

        let t = insert_thread(
            &conn,
            conv,
            "customer",
            "Where is my order?",
            "customer",
            None,
        );
        insert_event(
            &conn,
            conv,
            Some(t),
            "customer_message",
            "customer",
            None,
            "{}",
            "thread:9001",
        );
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        assert_eq!(count_type(&conn, "customer_replied"), 1);

        let (title, body, target, dedup): (String, Option<String>, Option<i64>, String) = conn
            .query_row(
                "SELECT title, body, target_user_id, dedup_key FROM notifications WHERE type = 'customer_replied'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert!(
            title.starts_with("Customer replied on #1001 — Printer"),
            "{title}"
        );
        assert_eq!(body.as_deref(), Some("Where is my order?"));
        assert_eq!(target, Some(42), "targeted at the assignee");
        assert_eq!(dedup, "n:evt:customer_replied:thread:9001");

        // Re-sweep is idempotent (cursor advanced, dedup stable).
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0);
        assert_eq!(count_type(&conn, "customer_replied"), 1);
    }

    #[test]
    fn assignment_observation_notifies_the_new_assignee_not_the_actor() {
        let conn = fresh_db();
        let conv = insert_conversation(&conn, 1001, None);
        conn.execute(
            "INSERT INTO users (id, remote_id, first_name, last_name, user_type) VALUES (42, 42, 'Alex', 'Rivera', 'user')",
            [],
        )
        .unwrap();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        // A sync observation diff: actor unknown (system), next = 42.
        insert_event(
            &conn,
            conv,
            None,
            "assignment_changed",
            "user",
            None,
            r#"{"previous":null,"next":42,"observed":true}"#,
            "assignment_changed:1001:2026-01-02T10:00:00.000Z",
        );
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, target, dedup): (String, Option<i64>, String) = conn
            .query_row(
                "SELECT title, target_user_id, dedup_key FROM notifications WHERE type = 'ticket_assigned'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "#1001 assigned to Alex Rivera");
        assert_eq!(target, Some(42));
        assert!(dedup.starts_with("n:evt:ticket_assigned:assignment_changed:1001:"));

        // Self-assignment (next == actor) never notifies.
        insert_event(
            &conn,
            conv,
            None,
            "assignment_changed",
            "user",
            Some(42),
            r#"{"previous":null,"next":42,"observed":true}"#,
            "assignment_changed:1001:2026-01-03T10:00:00.000Z",
        );
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0, "next == actor → no notification");
        // An assignment event without metadata.next (lineitem-derived in the
        // reference) never notifies either.
        insert_event(
            &conn,
            conv,
            None,
            "assignment_changed",
            "user",
            None,
            "{}",
            "assignment_changed:1001:2026-01-04T10:00:00.000Z",
        );
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0, "no next → no notification");
    }

    #[test]
    fn internal_note_mentions_notify_mentioned_users_not_the_author() {
        let conn = fresh_db();
        let conv = insert_conversation(&conn, 1001, None);
        conn.execute(
            "INSERT INTO users (id, remote_id, first_name, last_name, mention, user_type) VALUES (42, 42, 'Alex', 'Rivera', 'alex', 'user')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, remote_id, first_name, last_name, mention, user_type) VALUES (43, 43, 'Priya', 'Nair', 'priya', 'user')",
            [],
        )
        .unwrap();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        let note = "Checked with @priya — she owns the billing queue.";
        let t = insert_thread(&conn, conv, "note", note, "user", Some(42));
        insert_event(
            &conn,
            conv,
            Some(t),
            "internal_note",
            "user",
            Some(42),
            "{}",
            "thread:9002",
        );
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, target, actor, dedup): (String, Option<i64>, Option<i64>, String) = conn
            .query_row(
                "SELECT title, target_user_id, actor_user_local_id, dedup_key FROM notifications WHERE type = 'mentioned'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(title, "Alex Rivera mentioned you on #1001");
        assert_eq!(target, Some(43), "Priya was mentioned");
        assert_eq!(actor, Some(42), "Alex wrote the note");
        assert_eq!(dedup, "n:mention:thread:9002:43");

        // The author mentioning themselves produces nothing.
        let t2 = insert_thread(&conn, conv, "note", "Note to @alex self.", "user", Some(42));
        insert_event(
            &conn,
            conv,
            Some(t2),
            "internal_note",
            "user",
            Some(42),
            "{}",
            "thread:9003",
        );
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0);
    }

    // ---- 2. SLA risk / breach ---------------------------------------------

    #[test]
    fn sla_alerts_produce_risk_and_breach_notifications_with_day_dedup() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO users (id, remote_id, first_name, user_type)
             VALUES (42, 42, 'Alex', 'user')",
            [],
        )
        .unwrap();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        // One breached + one at-risk conversation in a configured mailbox
        // (the alerts engine walks the mailboxes mirror, then its config).
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (101, 101, 'Support')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mailbox_business_hours (mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min, updated_at)
             VALUES (101, 'UTC', '[0,1,2,3,4,5,6]', 0, 1440, 60, 240, datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO customers (remote_id, first_name) VALUES (2001, 'Em')",
            [],
        )
        .unwrap();
        // Breached: last CUSTOMER message 3 days ago, no reply (the SLA
        // clock starts at the last customer thread, not the conversation
        // stamp — so the thread carries the -3-days time).
        let c1 = insert_conversation(&conn, 1001, Some(42));
        conn.execute(
            "UPDATE conversations SET mailbox_id = 101, created_at = datetime('now', '-3 days'), updated_at = datetime('now', '-3 days') WHERE id = ?1",
            params![c1],
        )
        .unwrap();
        let t1 = insert_thread(&conn, c1, "customer", "help", "customer", None);
        conn.execute(
            "UPDATE conversation_threads SET created_at = datetime('now', '-3 days') WHERE id = ?1",
            params![t1],
        )
        .unwrap();
        // At risk: waited ~half the 60-minute target.
        let c2 = insert_conversation(&conn, 1002, Some(42));
        conn.execute(
            "UPDATE conversations SET mailbox_id = 101, created_at = datetime('now', '-50 minutes'), updated_at = datetime('now', '-50 minutes') WHERE id = ?1",
            params![c2],
        )
        .unwrap();
        let t2 = insert_thread(&conn, c2, "customer", "help too", "customer", None);
        conn.execute(
            "UPDATE conversation_threads SET created_at = datetime('now', '-50 minutes') WHERE id = ?1",
            params![t2],
        )
        .unwrap();

        let out = sweep(&conn, None).unwrap();
        assert!(out.created >= 2, "risk + breach, got {out:?}");
        let breach: Option<(String, String)> = conn
            .query_row(
                "SELECT title, severity FROM notifications WHERE type = 'sla_breach'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let (title, severity) = breach.expect("breach row");
        assert!(title.starts_with("SLA breached on #1001 ("), "{title}");
        assert!(title.contains("min over target"));
        assert_eq!(severity, "critical");
        let (risk_title, risk_severity): (String, String) = conn
            .query_row(
                "SELECT title, severity FROM notifications WHERE type = 'sla_risk'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(risk_title.starts_with("SLA at risk on #1002 ("));
        assert_eq!(risk_severity, "warning");

        // Same day re-sweep: the day-stamped dedup key holds.
        let out = sweep(&conn, None).unwrap();
        assert_eq!(
            count_type(&conn, "sla_breach"),
            1,
            "one breach notification per subject per day"
        );
        assert_eq!(count_type(&conn, "sla_risk"), 1);
        assert_eq!(out.created, 0);
    }

    // ---- 3. automation approvals -------------------------------------------

    #[test]
    fn automation_approval_jobs_notify_with_action_kind() {
        let conn = fresh_db();
        let conv = insert_conversation(&conn, 1001, None);
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        conn.execute(
            "INSERT INTO jobs (queue, type, priority, status, payload, run_at)
             VALUES ('ai', 'automation_action_awaiting_approval', 2, 'queued',
                     ?1, datetime('now'))",
            params![format!(
                r#"{{"ruleId":1,"conversationId":{conv},"action":{{"kind":"add_note"}}}}"#
            )],
        )
        .unwrap();
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, job_id, dedup): (String, Option<i64>, String) = conn
            .query_row(
                "SELECT title, job_id, dedup_key FROM notifications WHERE type = 'automation_approval'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(
            title.starts_with("Automation approval required: add_note on #1001"),
            "{title}"
        );
        assert_eq!(job_id, Some(1));
        assert_eq!(dedup, "n:approval:1");

        // Re-sweep: same job → no duplicate (the job is still parked).
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0);
    }

    // ---- 4. failed jobs -----------------------------------------------------

    #[test]
    fn failed_jobs_notify_only_when_completed_after_the_last_sweep() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap(); // sweep_at = now

        // A job that failed BEFORE the sweep stamp → already seen.
        conn.execute(
            "INSERT INTO jobs (queue, type, status, error, completed_at, run_at)
             VALUES ('sync', 'reindex', 'failed', 'boom', datetime('now', '-1 hour'), datetime('now'))",
            [],
        )
        .unwrap();
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0, "old failure is history");

        // A job that failed AFTER the sweep stamp → notify.
        conn.execute(
            "INSERT INTO jobs (queue, type, status, error, completed_at, run_at)
             VALUES ('ai', 'embed', 'failed', 'timeout', datetime('now', '+1 minute'), datetime('now'))",
            [],
        )
        .unwrap();
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, body, dedup): (String, Option<String>, String) = conn
            .query_row(
                "SELECT title, body, dedup_key FROM notifications WHERE type = 'job_failure'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Background job failed (ai/embed)");
        assert_eq!(body.as_deref(), Some("timeout"));
        assert_eq!(dedup, "n:jobfail:2");
    }

    // ---- 5. sync state ------------------------------------------------------

    #[test]
    fn sync_error_state_notifies_once_per_day() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        crate::sync_engine::set_state(&conn, "ERROR");
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, severity, body): (String, String, Option<String>) = conn
            .query_row(
                "SELECT title, severity, body FROM notifications WHERE type = 'sync_failure'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Help Scout sync is in ERROR state");
        assert_eq!(severity, "critical");
        assert!(body.unwrap().contains("Sync Health"));

        // Same day → deduped.
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 0);
        assert_eq!(count_type(&conn, "sync_failure"), 1);
    }

    // ---- 6. campaign replies ------------------------------------------------

    #[test]
    fn campaign_replies_notify_with_customer_and_campaign_names() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        conn.execute(
            "INSERT INTO customers (remote_id, first_name, last_name) VALUES (3001, 'Emma', 'Lindqvist')",
            [],
        )
        .unwrap();
        let emma: i64 = conn
            .query_row("SELECT id FROM customers WHERE remote_id = 3001", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO outreach_campaigns (name, subject, body) VALUES ('Winter check-in', 'How is it going?', 'hello')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outreach_recipients (campaign_id, customer_local_id, state, replied_at)
             VALUES (1, ?1, 'replied', datetime('now', '+1 minute'))",
            [emma],
        )
        .unwrap();
        // An OLD reply (before the sweep stamp) must not notify — a second
        // customer, since (campaign, customer) is UNIQUE.
        conn.execute(
            "INSERT INTO customers (remote_id, first_name, last_name) VALUES (3002, 'Old', 'Timer')",
            [],
        )
        .unwrap();
        let old_timer: i64 = conn
            .query_row("SELECT id FROM customers WHERE remote_id = 3002", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO outreach_recipients (campaign_id, customer_local_id, state, replied_at)
             VALUES (1, ?1, 'replied', datetime('now', '-2 days'))",
            [old_timer],
        )
        .unwrap();

        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, body, customer, campaign, dedup): (
            String,
            Option<String>,
            Option<i64>,
            Option<i64>,
            String,
        ) = conn
            .query_row(
                "SELECT title, body, customer_local_id, campaign_id, dedup_key FROM notifications WHERE type = 'campaign_reply'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            title,
            "Campaign reply: Emma Lindqvist answered \"Winter check-in\""
        );
        assert_eq!(body.as_deref(), Some("How is it going?"));
        assert_eq!(customer, Some(emma));
        assert_eq!(campaign, Some(1));
        assert_eq!(dedup, "n:campreply:1");
    }

    // ---- 7. known issues ----------------------------------------------------

    #[test]
    fn fresh_known_issues_notify_with_status_body() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        conn.execute(
            "INSERT INTO known_issues (name, status, created_at) VALUES ('Login loop', 'investigating', datetime('now', '+1 minute'))",
            [],
        )
        .unwrap();
        // A known issue from before the sweep stamp is history.
        conn.execute(
            "INSERT INTO known_issues (name, status, created_at) VALUES ('Old issue', 'resolved', datetime('now', '-3 days'))",
            [],
        )
        .unwrap();
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, body, issue_id, dedup): (String, Option<String>, Option<i64>, String) = conn
            .query_row(
                "SELECT title, body, issue_id, dedup_key FROM notifications WHERE type = 'known_issue_detected'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(title, "Known issue tracked: Login loop");
        assert_eq!(body.as_deref(), Some("Status: investigating"));
        assert_eq!(issue_id, Some(1));
        assert_eq!(dedup, "n:knownissue:1");
    }

    // ---- 8. issue spikes ----------------------------------------------------

    #[test]
    fn rising_clusters_notify_with_conversation_count() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        conn.execute(
            "INSERT INTO issue_clusters (name, conversation_count, trend, updated_at)
             VALUES ('DST reports', 7, 'rising', datetime('now', '+1 minute'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO issue_clusters (name, conversation_count, trend, updated_at)
             VALUES ('Stable thing', 7, 'stable', datetime('now', '+1 minute'))",
            [],
        )
        .unwrap();
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1, "only rising clusters notify");
        let (title, severity, dedup): (String, String, String) = conn
            .query_row(
                "SELECT title, severity, dedup_key FROM notifications WHERE type = 'issue_spike'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Issue spike: DST reports (7 conversations)");
        assert_eq!(severity, "warning");
        assert_eq!(dedup, format!("n:spike:1:{}", day_stamp()));
    }

    // ---- 9. AI escalations --------------------------------------------------

    #[test]
    fn ai_escalation_fires_on_urgency_with_confidence_and_skips_closed() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        let open_conv = insert_conversation(&conn, 1001, None);
        let closed_conv = insert_conversation(&conn, 1002, None);
        conn.execute(
            "UPDATE conversations SET status = 'closed' WHERE id = ?1",
            params![closed_conv],
        )
        .unwrap();
        let low_conf = insert_conversation(&conn, 1003, None);

        let mk_run = |conv: i64, output: &str| {
            conn.execute(
                "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type, conversation_id, status, created_at)
                 VALUES (?1, 'ticket_analysis_v1', 'demo_seed', ?2, 'ticket_analysis', ?3, 'completed', datetime('now', '+1 minute'))",
                params![format!("h{conv}"), output, conv],
            )
            .unwrap();
        };
        mk_run(
            open_conv,
            r#"{"urgency":"critical","sentiment":"negative","confidence":"high"}"#,
        );
        // Closed conversation: escalation on a closed ticket is noise.
        mk_run(
            closed_conv,
            r#"{"urgency":"critical","sentiment":"negative","confidence":"high"}"#,
        );
        // Low confidence: never escalate on a guess.
        mk_run(
            low_conf,
            r#"{"urgency":"high","sentiment":"negative","confidence":"low"}"#,
        );

        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, body, dedup): (String, Option<String>, String) = conn
            .query_row(
                "SELECT title, body, dedup_key FROM notifications WHERE type = 'ai_escalation'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "AI escalation on #1001 — urgency: critical");
        assert_eq!(
            body.as_deref(),
            Some("Printer on fire · AI confidence: high")
        );
        assert_eq!(dedup, "n:aiescal:1");
    }

    #[test]
    fn ai_escalation_fires_on_frustrated_sentiment() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();
        let conv = insert_conversation(&conn, 1001, None);
        conn.execute(
            "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type, conversation_id, status, created_at)
             VALUES ('h1', 'v', 'm', ?1, 'ticket_analysis', ?2, 'completed', datetime('now', '+1 minute'))",
            params![r#"{"urgency":"normal","sentiment":"frustrated","confidence":"medium"}"#, conv],
        )
        .unwrap();
        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let title: String = conn
            .query_row(
                "SELECT title FROM notifications WHERE type = 'ai_escalation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "AI escalation on #1001 — customer frustrated");
    }

    // ---- 10. ratings --------------------------------------------------------

    #[test]
    fn not_good_ratings_notify_as_customer_events() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        sweep(&conn, None).unwrap();

        conn.execute(
            "INSERT INTO customers (remote_id, first_name, last_name) VALUES (3001, 'Chloe', 'Dubois')",
            [],
        )
        .unwrap();
        let chloe: i64 = conn
            .query_row("SELECT id FROM customers WHERE remote_id = 3001", [], |r| {
                r.get(0)
            })
            .unwrap();
        let conv = insert_conversation(&conn, 1001, None);
        conn.execute(
            "INSERT INTO ratings (remote_id, conversation_id, rating, comments, customer_local_id, remote_created_at)
             VALUES (608, ?1, 'not-good', 'Still broken after the fix.', ?2, datetime('now', '+1 minute'))",
            params![conv, chloe],
        )
        .unwrap();
        // A great rating + an OLD not-good rating stay silent.
        conn.execute(
            "INSERT INTO ratings (remote_id, conversation_id, rating, comments, customer_local_id, remote_created_at)
             VALUES (609, ?1, 'great', NULL, ?2, datetime('now', '+1 minute'))",
            params![conv, chloe],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ratings (remote_id, conversation_id, rating, comments, customer_local_id, remote_created_at)
             VALUES (610, ?1, 'not-good', 'old', ?2, datetime('now', '-5 days'))",
            params![conv, chloe],
        )
        .unwrap();

        let out = sweep(&conn, None).unwrap();
        assert_eq!(out.created, 1);
        let (title, body, customer, dedup): (String, Option<String>, Option<i64>, String) = conn
            .query_row(
                "SELECT title, body, customer_local_id, dedup_key FROM notifications WHERE type = 'customer_event'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(title, "Not-good rating received from Chloe Dubois");
        assert_eq!(body.as_deref(), Some("Still broken after the fix."));
        assert_eq!(customer, Some(chloe));
        assert_eq!(dedup, "n:rating:608");
    }

    // ---- helpers ------------------------------------------------------------

    #[test]
    fn before_or_equal_handles_both_stamp_formats() {
        assert!(before_or_equal(
            "2026-01-01 10:00:00",
            "2026-01-01T10:00:00Z"
        ));
        assert!(before_or_equal(
            "2026-01-01T09:00:00Z",
            "2026-01-01 10:00:00"
        ));
        assert!(!before_or_equal(
            "2026-01-02 09:00:00",
            "2026-01-01 10:00:00"
        ));
        // Unparseable stamps count as seen (no spam).
        assert!(before_or_equal("garbage", "2026-01-01 10:00:00"));
    }

    #[test]
    fn trim_caps_with_ellipsis() {
        assert_eq!(trim("  hello  ", 10), "hello");
        assert_eq!(trim("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn me_resolution_uses_me_remote_id() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO users (remote_id, first_name) VALUES (1001, 'Alex')",
            [],
        )
        .unwrap();
        crate::settings::set_string(&conn, "me_remote_id", "1001").unwrap();
        assert_eq!(me_user_local_id(&conn).unwrap(), Some(1));
    }

    #[test]
    fn unread_count_includes_broadcast_rows() {
        let conn = fresh_db();
        record_notification(
            &conn,
            None,
            &NotificationInput {
                notification_type: NotificationType::IssueSpike,
                title: "spike".into(),
                dedup_key: "n:x:1".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(unread_count(&conn, Some(999)).unwrap(), 1);
    }
}
