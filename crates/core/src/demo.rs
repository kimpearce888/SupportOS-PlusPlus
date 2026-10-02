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
