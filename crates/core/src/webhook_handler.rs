//! Webhook processing pipeline — mirrors the reference
//! `src/server/services/webhookEndpoint.ts` exactly.
//!
//! Reference pipeline (`handle`):
//!   1. Read the raw body + `X-Helpscout-Signature` header.
//!   2. If a secret is configured: verify the **base64** HMAC-SHA1 over the
//!      RAW body (timing-safe). Invalid/missing signature → 401, not persisted.
//!      If NO secret is configured: accepted without verification (the
//!      reference logs a startup warning; same policy here).
//!   3. Persist with dedup: `event_hash = sha256("{eventType}:{payload}")`.
//!      A duplicate → state 'duplicate' + 200 `{received:true,duplicate:true}`.
//!   4. Prune the table to the newest 5,000 rows.
//!   5. Process asynchronously (never inside the HTTP request):
//!      state 'processing' → parse → extract conversationId
//!      (`objectID ?? id ?? conversationId`, finite positive numbers only) →
//!      switch on the event type → enqueue the matching sync job →
//!      state 'processed' (or 'failed' with the error).
//!   6. Return 200 `{received:true}`.
//!
//! Job kinds and priorities match the reference
//! (`JobRepository.enqueue(queue, kind, payload, priority, attempts)`):
//!   - 10 `convo.*` event types → `sync_conversation` (prio 2, 3 attempts)
//!   - `convo.merged`          → `sync_conversation_merge` (2, 3)
//!   - `convo.deleted`         → `delete_conversation` (2, 1)
//!   - `customer.created/updated` → `sync_customer` (2, 3)
//!   - `customer.deleted`      → `delete_customer` (2, 1)
//!   - `organization.*`        → `sync_organizations` (3, 2)
//!   - `satisfaction.ratings`  → `sync_conversation_ratings` (3, 2)
//!   - `tag.*`                 → `sync_tags` (3, 2)
//!   - `user.status.changed`   → `sync_user_statuses` (3, 2)
//!   - anything else           → recorded but not processed
//!
//! Boot drain (`drain_pending`): pending/failed events with attempts < 5,
//! up to 50 at a time.

use rusqlite::Connection;

use crate::jobs;
use crate::webhook;

/// The result of processing a webhook event (drives the HTTP response).
#[derive(Debug, PartialEq)]
pub enum WebhookProcessResult {
    /// New event: persisted, verified (or no secret configured), enqueued.
    Accepted { row_id: i64 },
    /// Duplicate (already in `webhook_events` by event hash).
    Duplicate { row_id: i64 },
    /// The HMAC signature verification failed → 401, NOT persisted.
    SignatureInvalid,
    /// The body was empty or not valid JSON → 400, NOT persisted.
    BadRequest,
}

/// Process a webhook event: verify → persist (dedup) → prune → enqueue.
///
/// `secret` is the configured webhook secret (empty = accept without
/// verification, matching the reference policy). `provided_signature` is the
/// raw `X-Helpscout-Signature` header value, if present. `event_type` is the
/// `X-HelpScout-Event` header value ("unknown" when absent) — the reference
/// reads the event type from the HEADER, never from the body.
pub fn process_webhook(
    conn: &Connection,
    secret: &[u8],
    body: &[u8],
    provided_signature: Option<&str>,
    event_type: &str,
) -> WebhookProcessResult {
    if body.is_empty() {
        return WebhookProcessResult::BadRequest;
    }
    // The reference treats an unparseable body as a client error (400) via
    // its content-type parser.
    if serde_json::from_slice::<serde_json::Value>(body).is_err() {
        return WebhookProcessResult::BadRequest;
    }

    // 1. Verify the signature FIRST (reference order). Only enforced when a
    //    secret is configured; without one the event is accepted unsigned.
    if !secret.is_empty() {
        let valid = match provided_signature {
            Some(sig) => webhook::verify_signature(secret, body, sig).is_ok(),
            None => false,
        };
        if !valid {
            return WebhookProcessResult::SignatureInvalid;
        }
    }

    // 2. Persist with the reference dedup key. The reference leaves the
    //    `event_id` column NULL (dedup is purely the sha256 hash).
    let raw = String::from_utf8_lossy(body).into_owned();
    let (row_id, duplicate) = match webhook::insert_webhook_event(conn, event_type, &raw, None) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "failed to persist webhook event");
            return WebhookProcessResult::BadRequest;
        }
    };
    if duplicate {
        let _ = webhook::set_webhook_event_state(conn, row_id, "duplicate", None);
        return WebhookProcessResult::Duplicate { row_id };
    }

    // 3. Bound the table (rate-limit-exempt endpoint).
    if let Err(e) = webhook::prune_webhook_events(conn, 5000) {
        tracing::warn!(error = %e, "failed to prune webhook_events");
    }

    // 4. Enqueue the matching sync job (synchronously here — the HTTP route
    //    acknowledges after this returns; the actual sync work runs in the
    //    job runner, never inside the request).
    process_event(conn, row_id, event_type, &raw);
    WebhookProcessResult::Accepted { row_id }
}

/// Idempotent event processing: state transitions + job fan-out.
/// Mirrors the reference `processEvent` exactly.
pub fn process_event(conn: &Connection, row_id: i64, event_type: &str, raw: &str) {
    if let Err(e) = webhook::set_webhook_event_state(conn, row_id, "processing", None) {
        tracing::error!(error = %e, "failed to mark webhook event processing");
        return;
    }

    let payload: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            let _ = webhook::set_webhook_event_state(
                conn,
                row_id,
                "failed",
                Some("Unparseable payload"),
            );
            return;
        }
    };

    // v1.6.0 audit fix parity: only finite positive numbers are treated as
    // conversation/customer ids.
    let conversation_id = positive_number(payload.get("objectID"))
        .or_else(|| positive_number(payload.get("id")))
        .or_else(|| positive_number(payload.get("conversationId")));

    let mut enqueued: Option<(&str, serde_json::Value)> = None;
    match event_type {
        "convo.created"
        | "convo.updated"
        | "convo.assigned"
        | "convo.status"
        | "convo.tags"
        | "convo.custom-fields"
        | "convo.moved"
        | "convo.customer.reply.created"
        | "convo.agent.reply.created"
        | "convo.note.created"
        | "convo.ai-answers.created" => {
            if let Some(id) = conversation_id {
                enqueued = Some((
                    "sync_conversation",
                    serde_json::json!({"remoteId": id, "source": "webhook"}),
                ));
            }
        }
        "convo.merged" => {
            if let Some(id) = conversation_id {
                enqueued = Some((
                    "sync_conversation_merge",
                    serde_json::json!({"remoteId": id}),
                ));
            }
        }
        "convo.deleted" => {
            if let Some(id) = conversation_id {
                enqueued = Some(("delete_conversation", serde_json::json!({"remoteId": id})));
            }
        }
        "customer.created" | "customer.updated" => {
            if let Some(id) = conversation_id {
                enqueued = Some(("sync_customer", serde_json::json!({"remoteId": id})));
            }
        }
        "customer.deleted" => {
            if let Some(id) = conversation_id {
                enqueued = Some(("delete_customer", serde_json::json!({"remoteId": id})));
            }
        }
        "organization.created" | "organization.updated" | "organization.deleted" => {
            enqueued = Some(("sync_organizations", serde_json::json!({})));
        }
        "satisfaction.ratings" => {
            let rating_id = positive_number(payload.get("ratingId"))
                .or_else(|| positive_number(payload.get("rating_id")))
                .or_else(|| positive_number(payload.get("id")));
            enqueued = Some((
                "sync_conversation_ratings",
                serde_json::json!({"conversationId": conversation_id, "ratingId": rating_id}),
            ));
        }
        "tag.created" | "tag.updated" | "tag.deleted" => {
            enqueued = Some(("sync_tags", serde_json::json!({})));
        }
        "user.status.changed" => {
            enqueued = Some(("sync_user_statuses", serde_json::json!({})));
        }
        _ => {
            // Unknown/other events are recorded but not processed.
        }
    }

    if let Some((kind, payload)) = enqueued {
        if let Err(e) = jobs::enqueue(conn, kind, &payload.to_string()) {
            tracing::error!(error = %e, kind, "failed to enqueue webhook job");
            let _ = webhook::set_webhook_event_state(conn, row_id, "failed", Some(&e.to_string()));
            return;
        }
    }

    let _ = webhook::set_webhook_event_state(conn, row_id, "processed", None);
}

/// Process leftover pending events (e.g. after restart). Returns how many
/// were drained. Mirrors the reference `drainPending` (limit 50).
pub fn drain_pending(conn: &Connection) -> usize {
    match webhook::get_pending_webhook_events(conn, 50) {
        Ok(pending) => {
            let count = pending.len();
            for ev in pending {
                process_event(conn, ev.id, &ev.event_type, &ev.payload);
            }
            count
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to query pending webhook events");
            0
        }
    }
}

/// Only finite positive numbers count as remote ids (reference v1.6.0 fix).
fn positive_number(v: Option<&serde_json::Value>) -> Option<i64> {
    let n = v?.as_f64()?;
    if n.is_finite() && n > 0.0 && n.fract() == 0.0 {
        Some(n as i64)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
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
        crate::webhook::ensure_webhook_events_table(&conn).unwrap();
        conn
    }

    fn good_signature(secret: &[u8], body: &[u8]) -> String {
        crate::webhook::compute_signature(secret, body)
    }

    fn job_count(conn: &Connection, kind: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind = ?1",
            params![kind],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn process_webhook_accepts_valid_event() {
        let conn = fresh_db();
        let secret = b"webhook_secret";
        let body = br#"{"id":"evt_001","type":"convo.created","objectID":1001}"#;
        let sig = good_signature(secret, body);

        let result = process_webhook(&conn, secret, body, Some(&sig), "convo.created");
        assert!(matches!(result, WebhookProcessResult::Accepted { .. }));
        // The event was persisted and (synchronously) processed.
        let (state, _count): (String, i64) = conn
            .query_row(
                "SELECT processing_state, COUNT(*) FROM webhook_events GROUP BY processing_state",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "processed");
        assert_eq!(job_count(&conn, "sync_conversation"), 1);
        // objectID was used as the remote id.
        let payload: String = conn
            .query_row(
                "SELECT payload FROM jobs WHERE kind = 'sync_conversation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(payload.contains("\"remoteId\":1001"));
        assert!(payload.contains("\"source\":\"webhook\""));
    }

    #[test]
    fn process_webhook_rejects_bad_signature_without_persisting() {
        let conn = fresh_db();
        let secret = b"webhook_secret";
        let body = br#"{"id":"evt_002","type":"convo.created"}"#;
        let bad_sig = "A".repeat(28); // wrong base64-length signature

        let result = process_webhook(&conn, secret, body, Some(&bad_sig), "convo.created");
        assert_eq!(result, WebhookProcessResult::SignatureInvalid);
        // Reference: 401 + NOT persisted.
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        assert_eq!(job_count(&conn, "sync_conversation"), 0);
    }

    #[test]
    fn process_webhook_rejects_missing_signature_when_secret_set() {
        let conn = fresh_db();
        let body = br#"{"id":"evt_003","type":"convo.created"}"#;
        let result = process_webhook(&conn, b"secret", body, None, "convo.created");
        assert_eq!(result, WebhookProcessResult::SignatureInvalid);
    }

    #[test]
    fn process_webhook_accepts_unsigned_when_no_secret() {
        // Reference policy: no secret configured → accepted WITHOUT verification.
        let conn = fresh_db();
        let body = br#"{"id":"evt_003b","type":"convo.created","objectID":7}"#;
        let result = process_webhook(&conn, b"", body, None, "convo.created");
        assert!(matches!(result, WebhookProcessResult::Accepted { .. }));
        assert_eq!(job_count(&conn, "sync_conversation"), 1);
    }

    #[test]
    fn process_webhook_dedups_by_event_type_and_payload_hash() {
        let conn = fresh_db();
        // The reference dedup key is sha256("{eventType}:{payload}") — a
        // REPLAY (same type + same raw payload) dedups, even though the
        // caller-supplied envelope ids differ.
        let raw = r#"{"id":"evt_a","type":"convo.created","objectID":5}"#;
        let (id1, dup1) =
            webhook::insert_webhook_event(&conn, "convo.created", raw, Some("evt_a")).unwrap();
        let (id2, dup2) =
            webhook::insert_webhook_event(&conn, "convo.created", raw, Some("evt_b")).unwrap();
        assert!(!dup1);
        assert!(dup2, "identical type+payload must dedup (replay)");
        assert_eq!(id1, id2, "duplicate resolves to the existing row id");

        // Different event type with the same body does NOT dedup.
        let (id3, dup3) =
            webhook::insert_webhook_event(&conn, "convo.updated", raw, Some("evt_c")).unwrap();
        assert!(!dup3);
        assert_ne!(id1, id3);

        // Different payload with the same event type does NOT dedup.
        let raw2 = r#"{"id":"evt_d","type":"convo.created","objectID":6}"#;
        let (_id4, dup4) =
            webhook::insert_webhook_event(&conn, "convo.created", raw2, Some("evt_d")).unwrap();
        assert!(!dup4);
    }

    #[test]
    fn process_webhook_rejects_invalid_json() {
        let conn = fresh_db();
        let result = process_webhook(
            &conn,
            b"secret",
            b"not json at all",
            Some("sig"),
            "convo.created",
        );
        assert_eq!(result, WebhookProcessResult::BadRequest);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn process_webhook_rejects_empty_body() {
        let conn = fresh_db();
        let result = process_webhook(&conn, b"secret", b"", Some("sig"), "convo.created");
        assert_eq!(result, WebhookProcessResult::BadRequest);
    }

    #[test]
    fn event_type_fan_out_matches_reference() {
        let conn = fresh_db();
        // Each (event type → job kind) pair from the reference switch.
        let cases: &[(&str, &str, bool)] = &[
            ("convo.created", "sync_conversation", true),
            ("convo.updated", "sync_conversation", true),
            ("convo.assigned", "sync_conversation", true),
            ("convo.status", "sync_conversation", true),
            ("convo.tags", "sync_conversation", true),
            ("convo.custom-fields", "sync_conversation", true),
            ("convo.moved", "sync_conversation", true),
            ("convo.customer.reply.created", "sync_conversation", true),
            ("convo.agent.reply.created", "sync_conversation", true),
            ("convo.note.created", "sync_conversation", true),
            ("convo.ai-answers.created", "sync_conversation", true),
            ("convo.merged", "sync_conversation_merge", true),
            ("convo.deleted", "delete_conversation", true),
            ("customer.created", "sync_customer", true),
            ("customer.updated", "sync_customer", true),
            ("customer.deleted", "delete_customer", true),
            ("organization.created", "sync_organizations", false),
            ("organization.updated", "sync_organizations", false),
            ("organization.deleted", "sync_organizations", false),
            ("satisfaction.ratings", "sync_conversation_ratings", false),
            ("tag.created", "sync_tags", false),
            ("tag.updated", "sync_tags", false),
            ("tag.deleted", "sync_tags", false),
            ("user.status.changed", "sync_user_statuses", false),
        ];
        for (i, (event_type, job_kind, needs_id)) in cases.iter().enumerate() {
            let body = if *needs_id {
                format!(
                    r#"{{"id":"e{i}","type":"{event_type}","objectID":{}}}"#,
                    i + 1
                )
            } else {
                format!(r#"{{"id":"e{i}","type":"{event_type}"}}"#)
            };
            let result = process_webhook(&conn, b"", body.as_bytes(), None, event_type);
            assert!(
                matches!(result, WebhookProcessResult::Accepted { .. }),
                "{event_type} should be accepted"
            );
            assert!(
                job_count(&conn, job_kind) >= 1,
                "job kind {job_kind} for {event_type}"
            );
        }
        // Count expected jobs per kind (several event types share a kind).
        let mut expected: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
        for (_event_type, job_kind, _needs_id) in cases.iter() {
            *expected.entry(job_kind).or_insert(0) += 1;
        }
        for (job_kind, count) in expected {
            assert_eq!(
                job_count(&conn, job_kind),
                count,
                "job kind {job_kind} total"
            );
        }
        // Unknown events: recorded, no job.
        let unknown = r#"{"id":"e_unk","type":"something.else"}"#;
        let result = process_webhook(&conn, b"", unknown.as_bytes(), None, "something.else");
        assert!(matches!(result, WebhookProcessResult::Accepted { .. }));
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            total,
            cases.len() as i64,
            "no extra jobs for unknown events"
        );
    }

    #[test]
    fn non_numeric_ids_are_not_treated_as_conversation_ids() {
        let conn = fresh_db();
        let body = br#"{"id":"evt_str","type":"convo.created","objectID":"not-a-number"}"#;
        let result = process_webhook(&conn, b"", body, None, "convo.created");
        assert!(matches!(result, WebhookProcessResult::Accepted { .. }));
        assert_eq!(
            job_count(&conn, "sync_conversation"),
            0,
            "string id must not enqueue"
        );
    }

    #[test]
    fn rating_event_carries_rating_id() {
        let conn = fresh_db();
        let body = br#"{"id":"evt_r","type":"satisfaction.ratings","objectID":42,"ratingId":99}"#;
        let result = process_webhook(&conn, b"", body, None, "satisfaction.ratings");
        assert!(matches!(result, WebhookProcessResult::Accepted { .. }));
        let payload: String = conn
            .query_row(
                "SELECT payload FROM jobs WHERE kind = 'sync_conversation_ratings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(payload.contains("\"conversationId\":42"));
        assert!(payload.contains("\"ratingId\":99"));
    }

    #[test]
    fn drain_pending_processes_leftover_events() {
        let conn = fresh_db();
        // Persist but leave in 'pending' state (simulate a crash between
        // insert and processing by inserting directly).
        let raw = r#"{"id":"evt_drain","type":"convo.created","objectID":3}"#;
        let (row_id, dup) =
            webhook::insert_webhook_event(&conn, "convo.created", raw, Some("evt_drain")).unwrap();
        assert!(!dup);

        let drained = drain_pending(&conn);
        assert_eq!(drained, 1);
        assert_eq!(job_count(&conn, "sync_conversation"), 1);
        let state: String = conn
            .query_row(
                "SELECT processing_state FROM webhook_events WHERE id = ?1",
                params![row_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "processed");
        // Second drain: nothing left.
        assert_eq!(drain_pending(&conn), 0);
    }

    #[test]
    fn failed_events_retry_until_attempt_cap() {
        let conn = fresh_db();
        let raw = r#"{"id":"evt_fail","type":"convo.created","objectID":1}"#;
        let (row_id, _) =
            webhook::insert_webhook_event(&conn, "convo.created", raw, Some("evt_fail")).unwrap();
        // Simulate 5 failed attempts.
        for _ in 0..5 {
            let _ = webhook::set_webhook_event_state(&conn, row_id, "failed", Some("boom"));
        }
        let pending = webhook::get_pending_webhook_events(&conn, 50).unwrap();
        assert!(pending.is_empty(), "attempts >= 5 must not be retried");
    }

    #[test]
    fn prune_keeps_newest_rows() {
        let conn = fresh_db();
        for i in 0..10 {
            let raw = format!(r#"{{"id":"e{i}","type":"convo.created","objectID":{i}}}"#);
            let _ =
                webhook::insert_webhook_event(&conn, "convo.created", &raw, Some(&format!("e{i}")))
                    .unwrap();
        }
        webhook::prune_webhook_events(&conn, 3).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
        let newest: i64 = conn
            .query_row("SELECT MAX(id) FROM webhook_events", [], |r| r.get(0))
            .unwrap();
        let oldest: i64 = conn
            .query_row("SELECT MIN(id) FROM webhook_events", [], |r| r.get(0))
            .unwrap();
        assert!(newest > oldest, "the newest rows must survive");
    }

    #[test]
    fn legacy_schema_is_forward_migrated() {
        let conn = fresh_db();
        // Create the OLD schema, then ensure the new one replaces it.
        conn.execute_batch(
            "DROP TABLE IF EXISTS webhook_events;
             CREATE TABLE webhook_events (
                 id TEXT PRIMARY KEY,
                 received_at TEXT NOT NULL,
                 body TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending',
                 processed_at TEXT
             );",
        )
        .unwrap();
        webhook::ensure_webhook_events_table(&conn).unwrap();
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(webhook_events)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(cols.contains(&"event_hash".to_string()));
        assert!(!cols.contains(&"body".to_string()));
    }
}
