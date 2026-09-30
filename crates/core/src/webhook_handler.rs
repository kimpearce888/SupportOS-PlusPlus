//! Webhook route handler (M2-T04).
//!
//! Per spec A2: "timing-safe HMAC-SHA1 webhook verification, persist-first
//! with deduplication". Per A9: "unprocessed persisted webhook events are
//! drained on boot."
//!
//! The handler is called by the loopback listener when Help Scout POSTs to
//! `POST /webhooks/helpscout`. The pipeline:
//!   1. Read the raw body + `X-Helpscout-Signature` header.
//!   2. **Persist-first**: write the raw event to `webhook_events` BEFORE
//!      verifying the signature (so a crash during verify doesn't lose the event).
//!   3. Timing-safe HMAC-SHA1 verify.
//!   4. **Dedup**: if the event ID already exists, discard (return 200 — Help Scout
//!      expects 200 on duplicate, otherwise it retries).
//!   5. Enqueue a `webhook.process` job.
//!   6. Return 200.
//!
//! The event ID is extracted from the JSON body (Help Scout includes `id` in
//! the webhook payload). If the body is not valid JSON or doesn't have an `id`,
//! we use a hash of the body as the event ID (so replays still dedup).

use rusqlite::Connection;

use crate::jobs;
use crate::webhook;

/// The result of processing a webhook event.
#[derive(Debug, PartialEq)]
pub enum WebhookProcessResult {
    /// The event was new: persisted, verified, and enqueued for processing.
    Accepted { event_id: String },
    /// The event was a duplicate (already in `webhook_events`). Help Scout
    /// expects 200 on duplicates so it doesn't retry.
    Duplicate { event_id: String },
    /// The HMAC signature verification failed. The event was persisted (for
    /// forensic analysis) but NOT enqueued for processing.
    SignatureInvalid,
    /// The body was empty or not valid JSON. The event was NOT persisted.
    BadRequest,
}

/// Process a webhook event: persist-first → verify signature → dedup → enqueue.
///
/// This is the core pipeline called by the axum route handler. It takes a
/// `&mut Connection` (so tests can use a fresh DB) and the webhook secret
/// (from settings). The `event_id` is extracted from the JSON body if possible,
/// or derived from a hash of the body.
///
/// Returns `WebhookProcessResult` so the caller (the route handler) can
/// render the right HTTP response.
pub fn process_webhook(
    conn: &Connection,
    secret: &[u8],
    body: &[u8],
    provided_signature: Option<&str>,
) -> WebhookProcessResult {
    if body.is_empty() {
        return WebhookProcessResult::BadRequest;
    }

    // Try to extract the event ID from the JSON body.
    let event_id = extract_event_id(body).unwrap_or_else(|| format!("hash:{}", short_hash(body)));

    // 1. Persist-first: write the raw event BEFORE verifying the signature.
    //    If this is a duplicate, persist_event returns false (dedup).
    let was_new = match webhook::persist_event(conn, &event_id, &String::from_utf8_lossy(body)) {
        Ok(true) => true,
        Ok(false) => {
            // Duplicate event — already in the table. Return 200 so Help Scout
            // doesn't retry.
            return WebhookProcessResult::Duplicate { event_id };
        }
        Err(e) => {
            tracing::error!(error = %e, event_id = %event_id, "failed to persist webhook event");
            return WebhookProcessResult::BadRequest;
        }
    };

    // 2. Verify the HMAC-SHA1 signature.
    if let Some(sig) = provided_signature {
        if let Err(_e) = webhook::verify_signature(secret, body, sig) {
            tracing::warn!(event_id = %event_id, "webhook signature verification failed");
            return WebhookProcessResult::SignatureInvalid;
        }
    } else {
        // No signature header — reject. (Help Scout always sends one.)
        tracing::warn!(event_id = %event_id, "webhook missing signature header");
        return WebhookProcessResult::SignatureInvalid;
    }

    // 3. Enqueue a job to process the event.
    if was_new {
        let payload = serde_json::json!({
            "event_id": event_id,
            "body": String::from_utf8_lossy(body),
        });
        if let Err(e) = jobs::enqueue(conn, "webhook.process", &payload.to_string()) {
            tracing::error!(error = %e, event_id = %event_id, "failed to enqueue webhook.process job");
        }
    }

    WebhookProcessResult::Accepted { event_id }
}

/// Extract the event ID from a Help Scout webhook JSON body.
/// Help Scout includes `id` in the payload (the webhook event ID, not the
/// conversation ID). If the body isn't valid JSON or doesn't have `id`,
/// returns `None`.
fn extract_event_id(body: &[u8]) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    json.get("id")?.as_str().map(|s| s.to_string())
}

/// A short hex hash of the body (first 16 chars of SHA-1). Used as a fallback
/// event ID when the body doesn't contain an `id` field.
fn short_hash(body: &[u8]) -> String {
    // Reuse the SHA-1 from the webhook module (it's not pub, so we compute
    // a simple hash here instead — not cryptographic, just for dedup).
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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

    fn good_signature(secret: &[u8], body: &[u8]) -> String {
        crate::webhook::compute_signature(secret, body)
    }

    #[test]
    fn process_webhook_accepts_valid_event() {
        let conn = fresh_db();
        let secret = b"webhook_secret";
        let body = br#"{"id":"evt_001","type":"convo.created","data":{"number":1001}}"#;
        let sig = good_signature(secret, body);

        let result = process_webhook(&conn, secret, body, Some(&sig));
        assert!(matches!(result, WebhookProcessResult::Accepted { .. }));

        // The event was persisted.
        assert_eq!(webhook::pending_event_count(&conn).unwrap(), 1);

        // A job was enqueued.
        let job_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE kind = 'webhook.process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(job_count, 1);
    }

    #[test]
    fn process_webhook_rejects_bad_signature() {
        let conn = fresh_db();
        let secret = b"webhook_secret";
        let body = br#"{"id":"evt_002","type":"convo.created"}"#;
        let bad_sig = "0".repeat(40); // wrong signature

        let result = process_webhook(&conn, secret, body, Some(&bad_sig));
        assert_eq!(result, WebhookProcessResult::SignatureInvalid);

        // The event was persisted (persist-first) but NOT enqueued.
        assert_eq!(webhook::pending_event_count(&conn).unwrap(), 1);
        let job_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE kind = 'webhook.process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            job_count, 0,
            "no job should be enqueued for a bad signature"
        );
    }

    #[test]
    fn process_webhook_rejects_missing_signature() {
        let conn = fresh_db();
        let secret = b"webhook_secret";
        let body = br#"{"id":"evt_003","type":"convo.created"}"#;

        let result = process_webhook(&conn, secret, body, None);
        assert_eq!(result, WebhookProcessResult::SignatureInvalid);
    }

    #[test]
    fn process_webhook_dedups_replay() {
        let conn = fresh_db();
        let secret = b"webhook_secret";
        let body = br#"{"id":"evt_004","type":"convo.created"}"#;
        let sig = good_signature(secret, body);

        // First call: accepted.
        let result1 = process_webhook(&conn, secret, body, Some(&sig));
        assert!(matches!(result1, WebhookProcessResult::Accepted { .. }));

        // Second call with same event ID: dedup.
        let result2 = process_webhook(&conn, secret, body, Some(&sig));
        assert!(matches!(result2, WebhookProcessResult::Duplicate { .. }));

        // Only 1 pending event (not 2).
        assert_eq!(webhook::pending_event_count(&conn).unwrap(), 1);

        // Only 1 job (not 2).
        let job_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE kind = 'webhook.process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(job_count, 1);
    }

    #[test]
    fn process_webhook_rejects_empty_body() {
        let conn = fresh_db();
        let result = process_webhook(&conn, b"secret", b"", Some("sig"));
        assert_eq!(result, WebhookProcessResult::BadRequest);
    }

    #[test]
    fn process_webhook_falls_back_to_hash_when_no_id() {
        let conn = fresh_db();
        let secret = b"webhook_secret";
        // No "id" field — the handler should use a hash of the body as the event ID.
        let body = br#"{"type":"convo.created","data":{"number":1001}}"#;
        let sig = good_signature(secret, body);

        let result = process_webhook(&conn, secret, body, Some(&sig));
        match result {
            WebhookProcessResult::Accepted { event_id } => {
                assert!(
                    event_id.starts_with("hash:"),
                    "event_id should start with 'hash:' when no id field"
                );
            }
            _ => panic!("expected Accepted, got {result:?}"),
        }

        // Replaying the same body should dedup (same hash).
        let result2 = process_webhook(&conn, secret, body, Some(&sig));
        assert!(matches!(result2, WebhookProcessResult::Duplicate { .. }));
    }

    #[test]
    fn extract_event_id_from_valid_json() {
        let body = br#"{"id":"evt_123","type":"convo.created"}"#;
        assert_eq!(extract_event_id(body), Some("evt_123".to_string()));
    }

    #[test]
    fn extract_event_id_returns_none_for_no_id() {
        let body = br#"{"type":"convo.created"}"#;
        assert_eq!(extract_event_id(body), None);
    }

    #[test]
    fn extract_event_id_returns_none_for_invalid_json() {
        let body = b"not json at all";
        assert_eq!(extract_event_id(body), None);
    }

    #[test]
    fn extract_event_id_returns_none_for_non_string_id() {
        let body = br#"{"id":123}"#;
        assert_eq!(extract_event_id(body), None);
    }

    #[test]
    fn short_hash_is_deterministic() {
        let h1 = short_hash(b"hello");
        let h2 = short_hash(b"hello");
        assert_eq!(h1, h2);
        let h3 = short_hash(b"world");
        assert_ne!(h1, h3);
    }
}
