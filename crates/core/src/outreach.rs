//! Outreach — segmentation, campaigns, do-not-contact, monitoring (M9).
//!
//! Per spec M9: "Outreach: segmentation, saved segments, campaigns,
//! do-not-contact, monitoring."
//! Per KNOWN PITFALLS: "Campaign recipients that exhaust retries must fail,
//! never livelock; 'retry failed' resets the attempt budget."

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::error::Result;

/// Migrations M023–M025.
pub const M023_TO_M025_SQL: &str = r#"
    -- M023: saved_segments
    CREATE TABLE IF NOT EXISTS saved_segments (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        name            TEXT NOT NULL,
        criteria_json   TEXT NOT NULL,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );

    -- M024: campaigns + campaign_recipients
    CREATE TABLE IF NOT EXISTS campaigns (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        name            TEXT NOT NULL,
        segment_id      INTEGER REFERENCES saved_segments (id),
        status          TEXT NOT NULL DEFAULT 'draft',
        message_template TEXT,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        started_at      TEXT,
        completed_at    TEXT
    );
    CREATE TABLE IF NOT EXISTS campaign_recipients (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        campaign_id     INTEGER NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
        customer_id     INTEGER NOT NULL,
        status          TEXT NOT NULL DEFAULT 'pending',
        attempts        INTEGER NOT NULL DEFAULT 0,
        max_attempts    INTEGER NOT NULL DEFAULT 3,
        last_error      TEXT,
        sent_at         TEXT,
        replied_at      TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_campaign_recipients_campaign
        ON campaign_recipients (campaign_id, status);

    -- M025: do_not_contact
    CREATE TABLE IF NOT EXISTS do_not_contact (
        customer_id     INTEGER PRIMARY KEY,
        reason          TEXT NOT NULL DEFAULT 'manual',
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );

    UPDATE app_state SET schema_version = 25 WHERE id = 1;
"#;

pub fn apply_m023_to_m025(conn: &Connection) -> Result<()> {
    conn.execute_batch(M023_TO_M025_SQL)?;
    Ok(())
}

// ─── M9-T01: Saved segments ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSegment {
    pub id: Option<i64>,
    pub name: String,
    pub criteria: String,
    pub created_at: String,
}

pub fn create_segment(conn: &Connection, name: &str, criteria: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO saved_segments (name, criteria_json) VALUES (?1, ?2)",
        params![name, criteria],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_segments(conn: &Connection) -> Result<Vec<SavedSegment>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, criteria_json, created_at FROM saved_segments ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SavedSegment {
                id: r.get(0)?,
                name: r.get(1)?,
                criteria: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_segment(conn: &Connection, id: i64) -> Result<Option<SavedSegment>> {
    let row = conn
        .query_row(
            "SELECT id, name, criteria_json, created_at FROM saved_segments WHERE id = ?1",
            params![id],
            |r| {
                Ok(SavedSegment {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    criteria: r.get(2)?,
                    created_at: r.get(3)?,
                })
            },
        )
        .ok();
    Ok(row)
}

pub fn delete_segment(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn.execute("DELETE FROM saved_segments WHERE id = ?1", params![id])?;
    Ok(rows > 0)
}

// ─── M9-T02: Campaigns ───────────────────────────────────────────────────

/// The maximum number of send attempts per recipient.
/// Per KNOWN PITFALLS: recipients that exhaust retries must FAIL, never livelock.
pub const DEFAULT_MAX_CAMPAIGN_ATTEMPTS: i64 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Campaign {
    pub id: Option<i64>,
    pub name: String,
    pub segment_id: Option<i64>,
    pub status: String,
    pub message_template: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CampaignRecipient {
    pub id: Option<i64>,
    pub campaign_id: i64,
    pub customer_id: i64,
    pub status: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub last_error: Option<String>,
    pub sent_at: Option<String>,
    pub replied_at: Option<String>,
}

pub fn create_campaign(
    conn: &Connection,
    name: &str,
    segment_id: Option<i64>,
    message_template: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO campaigns (name, segment_id, message_template) VALUES (?1, ?2, ?3)",
        params![name, segment_id, message_template],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn add_campaign_recipient(
    conn: &Connection,
    campaign_id: i64,
    customer_id: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO campaign_recipients (campaign_id, customer_id) VALUES (?1, ?2)",
        params![campaign_id, customer_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn start_campaign(conn: &Connection, campaign_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE campaigns SET status = 'sending', started_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1 AND status = 'draft'",
        params![campaign_id],
    )?;
    Ok(rows > 0)
}

/// Mark a recipient as sent.
pub fn mark_recipient_sent(conn: &Connection, recipient_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE campaign_recipients SET status = 'sent', sent_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1 AND status = 'pending'",
        params![recipient_id],
    )?;
    Ok(rows > 0)
}

/// Mark a recipient as failed (after exhausting retries).
pub fn mark_recipient_failed(conn: &Connection, recipient_id: i64, error: &str) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE campaign_recipients SET status = 'failed', last_error = ?1
         WHERE id = ?2",
        params![error, recipient_id],
    )?;
    Ok(rows > 0)
}

/// Increment the attempt count for a recipient. Returns the new attempt count.
/// Per KNOWN PITFALLS: "recipients that exhaust retries must fail, never livelock."
pub fn increment_attempt(conn: &Connection, recipient_id: i64) -> Result<i64> {
    conn.execute(
        "UPDATE campaign_recipients SET attempts = attempts + 1 WHERE id = ?1",
        params![recipient_id],
    )?;
    let attempts: i64 = conn.query_row(
        "SELECT attempts FROM campaign_recipients WHERE id = ?1",
        params![recipient_id],
        |r| r.get(0),
    )?;
    Ok(attempts)
}

/// Whether a recipient has exhausted its retry budget.
/// Per KNOWN PITFALLS: "recipients that exhaust retries must fail, never livelock."
pub fn recipient_exhausted(conn: &Connection, recipient_id: i64) -> Result<bool> {
    let (attempts, max): (i64, i64) = conn.query_row(
        "SELECT attempts, max_attempts FROM campaign_recipients WHERE id = ?1",
        params![recipient_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(attempts >= max)
}

/// Retry failed recipients — resets their attempt budget so they get another chance.
/// Per KNOWN PITFALLS: "'retry failed' resets the attempt budget."
pub fn retry_failed_recipients(conn: &Connection, campaign_id: i64) -> Result<u32> {
    let rows = conn.execute(
        "UPDATE campaign_recipients
         SET status = 'pending', attempts = 0, last_error = NULL
         WHERE campaign_id = ?1 AND status = 'failed'",
        params![campaign_id],
    )?;
    Ok(u32::try_from(rows).unwrap_or(0))
}

/// Count active campaigns (status = 'sending'). Wires the `CampaignActivity`
/// Operations Center tile (M4-T01 stub → real count).
pub fn count_active_campaigns(conn: &Connection) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM campaigns WHERE status = 'sending'",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

/// List all campaigns (most recent first).
pub fn list_campaigns(conn: &Connection) -> Result<Vec<Campaign>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, segment_id, status, message_template, created_at, started_at, completed_at
         FROM campaigns ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Campaign {
            id: r.get(0)?,
            name: r.get(1)?,
            segment_id: r.get(2)?,
            status: r.get(3)?,
            message_template: r.get(4)?,
            created_at: r.get(5)?,
            started_at: r.get(6)?,
            completed_at: r.get(7)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.into())
}

// ─── M9-T03: Do-not-contact ──────────────────────────────────────────────

pub fn add_to_dnc(conn: &Connection, customer_id: i64, reason: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO do_not_contact (customer_id, reason) VALUES (?1, ?2)",
        params![customer_id, reason],
    )?;
    Ok(())
}

pub fn remove_from_dnc(conn: &Connection, customer_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "DELETE FROM do_not_contact WHERE customer_id = ?1",
        params![customer_id],
    )?;
    Ok(rows > 0)
}

pub fn is_on_dnc(conn: &Connection, customer_id: i64) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM do_not_contact WHERE customer_id = ?1)",
        params![customer_id],
        |r| r.get(0),
    )?;
    Ok(exists != 0)
}

pub fn list_dnc(conn: &Connection) -> Result<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT customer_id, reason, created_at FROM do_not_contact ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ─── M9-T04: Campaign monitoring ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CampaignStats {
    pub total_recipients: u32,
    pub sent: u32,
    pub failed: u32,
    pub pending: u32,
    pub replied: u32,
    pub reply_rate: f64,
}

pub fn get_campaign_stats(conn: &Connection, campaign_id: i64) -> Result<CampaignStats> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM campaign_recipients WHERE campaign_id = ?1",
        params![campaign_id],
        |r| r.get(0),
    )?;
    let sent: i64 = conn.query_row(
        "SELECT COUNT(*) FROM campaign_recipients WHERE campaign_id = ?1 AND status = 'sent'",
        params![campaign_id],
        |r| r.get(0),
    )?;
    let failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM campaign_recipients WHERE campaign_id = ?1 AND status = 'failed'",
        params![campaign_id],
        |r| r.get(0),
    )?;
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM campaign_recipients WHERE campaign_id = ?1 AND status = 'pending'",
        params![campaign_id],
        |r| r.get(0),
    )?;
    let replied: i64 = conn.query_row(
        "SELECT COUNT(*) FROM campaign_recipients WHERE campaign_id = ?1 AND replied_at IS NOT NULL",
        params![campaign_id],
        |r| r.get(0),
    )?;
    let total_u = u32::try_from(total).unwrap_or(0);
    let sent_u = u32::try_from(sent).unwrap_or(0);
    let replied_u = u32::try_from(replied).unwrap_or(0);
    let reply_rate = if sent_u > 0 {
        replied_u as f64 / sent_u as f64
    } else {
        0.0
    };
    Ok(CampaignStats {
        total_recipients: total_u,
        sent: sent_u,
        failed: u32::try_from(failed).unwrap_or(0),
        pending: u32::try_from(pending).unwrap_or(0),
        replied: replied_u,
        reply_rate,
    })
}

/// Mark a recipient as replied (campaign reply tracking).
pub fn mark_recipient_replied(conn: &Connection, recipient_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE campaign_recipients SET replied_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1 AND replied_at IS NULL",
        params![recipient_id],
    )?;
    Ok(rows > 0)
}

// ===========================================================================
// Campaign lifecycle service (reference src/server/outreach/campaignService.ts)
// ===========================================================================

/// Personalization variables (reference shared/segmentation.ts:356).
pub const PERSONALIZATION_VARIABLES: [&str; 6] = [
    "first_name",
    "last_name",
    "company",
    "organization",
    "last_ticket_number",
    "last_ticket_subject",
];

/// Hard per-recipient attempt cap (spec #31).
const MAX_ATTEMPTS: i64 = 3;

/// Apply the M031 batch: reference-shaped outreach tables (009_outreach).
pub fn apply_m031(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS outreach_campaigns (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            name            TEXT NOT NULL,
            subject         TEXT NOT NULL,
            body            TEXT NOT NULL,
            mailbox_local_id INTEGER,
            tags            TEXT NOT NULL DEFAULT '[]',
            status          TEXT NOT NULL DEFAULT 'draft',
            segment_id      INTEGER,
            segment_snapshot TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            queued_at       TEXT,
            completed_at    TEXT,
            updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS outreach_recipients (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            campaign_id     INTEGER NOT NULL,
            customer_local_id INTEGER NOT NULL,
            customer_remote_id INTEGER,
            email           TEXT,
            snapshot        TEXT NOT NULL DEFAULT '{}',
            state           TEXT NOT NULL DEFAULT 'selected',
            attempts        INTEGER NOT NULL DEFAULT 0,
            last_error      TEXT,
            hs_conversation_remote_id INTEGER,
            hs_conversation_number INTEGER,
            sent_at         TEXT,
            replied_at      TEXT,
            UNIQUE (campaign_id, customer_local_id)
        );
        CREATE INDEX IF NOT EXISTS idx_outreach_recipients_campaign
            ON outreach_recipients(campaign_id, state);
        CREATE TABLE IF NOT EXISTS outreach_attempts (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            recipient_id    INTEGER NOT NULL,
            attempt_no      INTEGER NOT NULL,
            started_at      TEXT NOT NULL,
            finished_at     TEXT,
            result          TEXT,
            error           TEXT
        );
        CREATE TABLE IF NOT EXISTS outreach_events (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            campaign_id     INTEGER NOT NULL,
            recipient_id    INTEGER,
            event           TEXT NOT NULL,
            detail          TEXT,
            at              TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_outreach_events_campaign
            ON outreach_events(campaign_id);
        -- Rebuild do_not_contact with a NULLABLE reason (reference 009:
        -- `reason TEXT`); the legacy port table had NOT NULL DEFAULT 'manual'
        -- which rejected the reference's explicit NULL inserts.
        CREATE TABLE IF NOT EXISTS do_not_contact_m031 (
            customer_id     INTEGER PRIMARY KEY,
            reason          TEXT,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        INSERT OR IGNORE INTO do_not_contact_m031 (customer_id, reason, created_at)
            SELECT customer_id, CASE WHEN reason = 'manual' THEN NULL ELSE reason END, created_at
              FROM do_not_contact;
        DROP TABLE IF EXISTS do_not_contact;
        ALTER TABLE do_not_contact_m031 RENAME TO do_not_contact;",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 31 WHERE id = 1", []);
    Ok(())
}

/// One campaign row with computed counters (reference CampaignDetail).
#[derive(Debug, Clone, Serialize)]
pub struct CampaignDetail {
    pub id: i64,
    pub name: String,
    pub subject: String,
    pub body: String,
    pub mailbox_local_id: Option<i64>,
    pub mailbox_name: Option<String>,
    pub tags: Vec<String>,
    pub status: String,
    pub segment_id: Option<i64>,
    pub segment_name: Option<String>,
    pub recipients: i64,
    pub sent: i64,
    pub failed: i64,
    pub skipped: i64,
    pub unknown: i64,
    pub replied: i64,
    pub created_at: String,
    pub queued_at: Option<String>,
    pub completed_at: Option<String>,
}

fn parse_tags(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn map_campaign_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CampaignDetail> {
    Ok(CampaignDetail {
        id: r.get(0)?,
        name: r.get(1)?,
        subject: r.get(2)?,
        body: r.get(3)?,
        mailbox_local_id: r.get(4)?,
        tags: parse_tags(&r.get::<_, String>(5).unwrap_or_default()),
        status: r.get(6)?,
        segment_id: r.get(7)?,
        created_at: r.get(8)?,
        queued_at: r.get(9)?,
        completed_at: r.get(10)?,
        mailbox_name: r.get(11)?,
        segment_name: r.get(12)?,
        recipients: r.get(13)?,
        sent: r.get(14)?,
        failed: r.get(15)?,
        skipped: r.get(16)?,
        unknown: r.get(17)?,
        replied: r.get(18)?,
    })
}

const CAMPAIGN_SELECT: &str = "SELECT oc.id, oc.name, oc.subject, oc.body, oc.mailbox_local_id, oc.tags,
        oc.status, oc.segment_id, oc.created_at, oc.queued_at, oc.completed_at,
        m.name, s.name,
        (SELECT COUNT(*) FROM outreach_recipients r WHERE r.campaign_id = oc.id),
        (SELECT COUNT(*) FROM outreach_recipients r WHERE r.campaign_id = oc.id AND r.state = 'sent'),
        (SELECT COUNT(*) FROM outreach_recipients r WHERE r.campaign_id = oc.id AND r.state = 'failed'),
        (SELECT COUNT(*) FROM outreach_recipients r WHERE r.campaign_id = oc.id AND r.state = 'skipped'),
        (SELECT COUNT(*) FROM outreach_recipients r WHERE r.campaign_id = oc.id AND r.state = 'unknown'),
        (SELECT COUNT(*) FROM outreach_recipients r WHERE r.campaign_id = oc.id AND r.replied_at IS NOT NULL)
   FROM outreach_campaigns oc
   LEFT JOIN mailboxes m ON m.id = oc.mailbox_local_id
   LEFT JOIN saved_segments s ON s.id = oc.segment_id";

/// `outreachRepo.getCampaign(id)`.
pub fn get_outreach_campaign(conn: &Connection, id: i64) -> Result<Option<CampaignDetail>> {
    let mut stmt = conn.prepare(&format!("{CAMPAIGN_SELECT} WHERE oc.id = ?1"))?;
    let mut rows = stmt.query_map([id], map_campaign_row)?;
    Ok(rows.next().transpose()?)
}

/// `outreachRepo.listEvents(campaignId)`.
pub fn list_campaign_events(conn: &Connection, campaign_id: i64) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, campaign_id, recipient_id, event, detail, at
           FROM outreach_events WHERE campaign_id = ?1 ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([campaign_id], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "campaign_id": r.get::<_, i64>(1)?,
            "recipient_id": r.get::<_, Option<i64>>(2)?,
            "event": r.get::<_, String>(3)?,
            "detail": r.get::<_, Option<String>>(4)?,
            "at": r.get::<_, String>(5)?,
        }))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.into())
}

fn log_event(
    conn: &Connection,
    campaign_id: i64,
    recipient_id: Option<i64>,
    event: &str,
    detail: Option<&str>,
) {
    let _ = conn.execute(
        "INSERT INTO outreach_events (campaign_id, recipient_id, event, detail)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![campaign_id, recipient_id, event, detail],
    );
}

fn update_status(conn: &Connection, id: i64, status: &str) {
    let _ = conn.execute(
        "UPDATE outreach_campaigns SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![status, id],
    );
}

fn count_remaining(conn: &Connection, campaign_id: i64) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM outreach_recipients
          WHERE campaign_id = ?1 AND state IN ('selected','queued','sending')",
        [campaign_id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Personalization context (reference campaignService.personalizationContext).
fn personalization_context(
    conn: &Connection,
    customer_local_id: i64,
    matching_tickets: &[serde_json::Value],
) -> serde_json::Value {
    let (first, last, org): (Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT c.first_name, c.last_name, o.name
               FROM customers c LEFT JOIN organizations o ON o.id = c.organization_id
              WHERE c.id = ?1",
            [customer_local_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((None, None, None));
    let last_ticket = matching_tickets.first();
    serde_json::json!({
        "first_name": first,
        "last_name": last,
        "company": org,
        "organization": org,
        "last_ticket_number": last_ticket.and_then(|t| t.get("number")).cloned().unwrap_or(serde_json::Value::Null),
        "last_ticket_subject": last_ticket.and_then(|t| t.get("subject")).cloned().unwrap_or(serde_json::Value::Null),
    })
}

/// Render `{{variables}}` (reference campaignService.render): unknown or
/// empty variables are reported in `unresolved`, never silently swallowed.
pub fn render_template(text: &str, ctx: &serde_json::Value) -> (String, Vec<String>) {
    let mut unresolved: Vec<String> = Vec::new();
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // find closing }}
            if let Some(close) = text[i + 2..].find("}}") {
                let inner = &text[i + 2..i + 2 + close];
                let raw_name = inner.trim();
                let name = raw_name.to_lowercase();
                if !raw_name.is_empty()
                    && raw_name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    if PERSONALIZATION_VARIABLES.contains(&name.as_str()) {
                        let v = &ctx[&name];
                        match v {
                            serde_json::Value::Null => {
                                unresolved.push(name.clone());
                            }
                            serde_json::Value::String(s) if s.is_empty() => {
                                unresolved.push(name.clone());
                            }
                            serde_json::Value::String(s) => out.push_str(s),
                            other => out.push_str(&other.to_string()),
                        }
                    } else {
                        unresolved.push(name);
                        out.push_str(&format!("{{{{{raw_name}}}}}"));
                    }
                    i = i + 2 + close + 2;
                    continue;
                }
            }
        }
        // advance one char (utf-8 safe)
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    (out, unresolved)
}

/// `renderFor` — subject + body for one recipient.
pub fn render_for(
    conn: &Connection,
    customer_local_id: i64,
    matching_tickets: &[serde_json::Value],
    subject: &str,
    body: &str,
) -> serde_json::Value {
    let ctx = personalization_context(conn, customer_local_id, matching_tickets);
    let (s, mut unresolved_s) = render_template(subject, &ctx);
    let (b, unresolved_b) = render_template(body, &ctx);
    unresolved_s.extend(unresolved_b);
    serde_json::json!({ "subject": s, "body": b, "unresolved": unresolved_s })
}

/// `campaignService.validate` (spec #27) — full recipient-scan validation.
pub fn campaign_validate(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    let Some(c) = get_outreach_campaign(conn, campaign_id).ok().flatten() else {
        return serde_json::json!({
            "ok": false,
            "errors": ["Campaign not found."],
            "warnings": [],
            "counts": { "recipients": 0, "ready": 0, "invalid_email": 0,
                        "already_sent": 0, "on_dnc": 0, "no_email": 0 }
        });
    };
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    if c.subject.trim().is_empty() {
        errors.push("Subject is empty.".into());
    }
    if c.body.trim().is_empty() {
        errors.push("Message body is empty.".into());
    }
    match c.mailbox_local_id {
        None => errors.push("No sending mailbox selected.".into()),
        Some(mid) => {
            let mailbox: Option<i64> = conn
                .query_row("SELECT id FROM mailboxes WHERE id = ?1", [mid], |r| {
                    r.get(0)
                })
                .ok();
            if mailbox.is_none() {
                errors.push("The selected mailbox no longer exists.".into());
            }
        }
    }
    let dnc: std::collections::HashSet<i64> = conn
        .prepare("SELECT customer_id FROM do_not_contact")
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([], |r| r.get::<_, i64>(0))
                .map(|rows| rows.filter_map(|x| x.ok()).collect())
                .ok()
        })
        .unwrap_or_default();
    let email_ok = |e: &str| {
        let parts: Vec<&str> = e.split('@').collect();
        parts.len() == 2
            && !parts[0].trim().is_empty()
            && parts[0] == parts[0].trim()
            && parts[1].split('.').count() >= 2
            && parts[1].split('.').all(|p| !p.trim().is_empty())
            && parts[1]
                .rsplit('.')
                .next()
                .is_some_and(|tld| tld.len() >= 2)
    };
    let mut ready = 0i64;
    let mut invalid_email = 0i64;
    let mut already_sent = 0i64;
    let mut on_dnc = 0i64;
    let mut no_email = 0i64;
    let rows: Vec<(i64, Option<String>, String)> = conn
        .prepare("SELECT customer_local_id, email, state FROM outreach_recipients WHERE campaign_id = ?1")
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([campaign_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map(|rows| rows.filter_map(|x| x.ok()).collect())
                .ok()
        })
        .unwrap_or_default();
    for (cid, email, state) in &rows {
        if state == "sent" {
            already_sent += 1;
        }
        if state == "sent" || state == "cancelled" {
            continue;
        }
        if dnc.contains(cid) {
            on_dnc += 1;
            continue;
        }
        match email.as_deref() {
            None => no_email += 1,
            Some(e) if !email_ok(e) => invalid_email += 1,
            Some(_) => ready += 1,
        }
    }
    if already_sent > 0 {
        warnings.push(format!("{already_sent} recipient(s) were already sent this campaign and will not be sent again."));
    }
    if on_dnc > 0 {
        warnings.push(format!(
            "{on_dnc} recipient(s) are on the Do-Not-Contact list and will be skipped."
        ));
    }
    if no_email > 0 {
        warnings.push(format!(
            "{no_email} recipient(s) have no usable email address and will be skipped."
        ));
    }
    if invalid_email > 0 {
        errors.push(format!(
            "{invalid_email} recipient(s) have an invalid email address."
        ));
    }
    if ready == 0 {
        errors.push("No recipients are ready to send.".into());
    }
    // Personalization sample check (first 25 recipients).
    let sample: Vec<(i64, String)> = conn
        .prepare(
            "SELECT customer_local_id, snapshot FROM outreach_recipients
              WHERE campaign_id = ?1 ORDER BY id LIMIT 25",
        )
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([campaign_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|rows| rows.filter_map(|x| x.ok()).collect())
                .ok()
        })
        .unwrap_or_default();
    let mut unresolved_vars: Vec<String> = Vec::new();
    for (cid, snapshot) in &sample {
        let tickets: Vec<serde_json::Value> = serde_json::from_str::<serde_json::Value>(snapshot)
            .ok()
            .and_then(|v| v.get("matching_tickets").cloned())
            .and_then(|t| serde_json::from_value(t).ok())
            .unwrap_or_default();
        let rendered = render_for(conn, *cid, &tickets, &c.subject, &c.body);
        for v in rendered["unresolved"].as_array().unwrap_or(&Vec::new()) {
            if let Some(name) = v.as_str() {
                if !unresolved_vars.contains(&name.to_string()) {
                    unresolved_vars.push(name.to_string());
                }
            }
        }
    }
    if !unresolved_vars.is_empty() {
        warnings.push(format!(
            "Personalization variable(s) with no value for some recipients: {} (they render as empty text).",
            unresolved_vars.join(", ")
        ));
    }
    serde_json::json!({
        "ok": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
        "counts": {
            "recipients": c.recipients, "ready": ready, "invalid_email": invalid_email,
            "already_sent": already_sent, "on_dnc": on_dnc, "no_email": no_email
        }
    })
}

/// `campaignService.queue` — validate, mark queued, enqueue the batch job.
pub fn campaign_queue(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    let Some(c) = get_outreach_campaign(conn, campaign_id).ok().flatten() else {
        return serde_json::json!({ "ok": false, "message": "Campaign not found." });
    };
    if c.status != "draft" && c.status != "paused" {
        return serde_json::json!({ "ok": false, "message": format!("Campaign is {}; only draft or paused campaigns can be queued.", c.status) });
    }
    let validation = campaign_validate(conn, campaign_id);
    if !validation["ok"].as_bool().unwrap_or(false) {
        let errors: Vec<String> = validation["errors"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        return serde_json::json!({ "ok": false, "message": format!("Validation failed: {}", errors.join(" ")) });
    }
    let ready = validation["counts"]["ready"].as_i64().unwrap_or(0);
    let recipients = validation["counts"]["recipients"].as_i64().unwrap_or(0);
    update_status(conn, campaign_id, "queued");
    let _ = conn.execute(
        "UPDATE outreach_campaigns SET queued_at = datetime('now') WHERE id = ?1",
        [campaign_id],
    );
    log_event(
        conn,
        campaign_id,
        None,
        "campaign_queued",
        Some(&format!("{ready} ready of {recipients}")),
    );
    let _ = crate::jobs::enqueue_on(
        conn,
        "outreach",
        "outreach_send_batch",
        &serde_json::json!({ "campaignId": campaign_id }).to_string(),
        1,
        3,
    );
    serde_json::json!({ "ok": true, "message": format!("Campaign queued: {ready} conversations will be created (one per customer).") })
}

/// `campaignService.pause`.
pub fn campaign_pause(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    let Some(c) = get_outreach_campaign(conn, campaign_id).ok().flatten() else {
        return serde_json::json!({ "ok": false, "message": "Campaign not found." });
    };
    if c.status != "queued" && c.status != "sending" {
        return serde_json::json!({ "ok": false, "message": format!("Only queued or sending campaigns can be paused (current: {}).", c.status) });
    }
    update_status(conn, campaign_id, "paused");
    log_event(conn, campaign_id, None, "campaign_paused", None);
    serde_json::json!({ "ok": true, "message": "Campaign paused. Already-sent recipients are untouched." })
}

/// `campaignService.resume`.
pub fn campaign_resume(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    let Some(c) = get_outreach_campaign(conn, campaign_id).ok().flatten() else {
        return serde_json::json!({ "ok": false, "message": "Campaign not found." });
    };
    if c.status != "paused" {
        return serde_json::json!({ "ok": false, "message": format!("Only paused campaigns can be resumed (current: {}).", c.status) });
    }
    update_status(conn, campaign_id, "queued");
    log_event(conn, campaign_id, None, "campaign_resumed", None);
    let _ = crate::jobs::enqueue_on(
        conn,
        "outreach",
        "outreach_send_batch",
        &serde_json::json!({ "campaignId": campaign_id }).to_string(),
        1,
        3,
    );
    serde_json::json!({ "ok": true, "message": "Campaign resumed." })
}

/// `campaignService.cancelRemaining`.
pub fn campaign_cancel_remaining(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    let Some(_c) = get_outreach_campaign(conn, campaign_id).ok().flatten() else {
        return serde_json::json!({ "ok": false, "message": "Campaign not found." });
    };
    let cancelled = conn
        .execute(
            "UPDATE outreach_recipients SET state = 'cancelled'
              WHERE campaign_id = ?1 AND state IN ('selected','queued','sending')",
            [campaign_id],
        )
        .unwrap_or(0) as i64;
    update_status(conn, campaign_id, "cancelled");
    log_event(
        conn,
        campaign_id,
        None,
        "campaign_cancelled",
        Some(&format!("{cancelled} remaining recipients cancelled")),
    );
    serde_json::json!({ "ok": true, "message": format!("{cancelled} remaining recipient(s) cancelled. Sent recipients are untouched.") })
}

/// `campaignService.retryFailed`.
pub fn campaign_retry_failed(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    let Some(_c) = get_outreach_campaign(conn, campaign_id).ok().flatten() else {
        return serde_json::json!({ "ok": false, "message": "Campaign not found." });
    };
    let reset = conn
        .execute(
            "UPDATE outreach_recipients SET state = 'queued', attempts = 0, last_error = NULL
              WHERE campaign_id = ?1 AND state = 'failed'",
            [campaign_id],
        )
        .unwrap_or(0) as i64;
    if reset > 0 {
        update_status(conn, campaign_id, "queued");
        log_event(
            conn,
            campaign_id,
            None,
            "campaign_retry_failed",
            Some(&format!(
                "{reset} recipients re-queued (attempt budget reset)"
            )),
        );
        let _ = crate::jobs::enqueue_on(
            conn,
            "outreach",
            "outreach_send_batch",
            &serde_json::json!({ "campaignId": campaign_id }).to_string(),
            1,
            3,
        );
    }
    serde_json::json!({ "ok": true, "message": if reset > 0 {
        format!("{reset} failed recipient(s) re-queued.")
    } else {
        "No failed recipients to retry.".to_string()
    }})
}

/// `campaignService.reconcile` — resolve UNKNOWN recipients against the local
/// mirror (demo mode: the local DB is the remote).
pub fn campaign_reconcile(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    /// (recipient id, customer remote, email, campaign subject)
    type UnknownRow = (i64, Option<i64>, Option<String>, Option<String>);
    let unknowns: Vec<UnknownRow> = conn
        .prepare(
            "SELECT r.id, r.customer_remote_id, r.email, oc.subject
               FROM outreach_recipients r
               JOIN outreach_campaigns oc ON oc.id = r.campaign_id
              WHERE r.campaign_id = ?1 AND r.state = 'unknown'",
        )
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([campaign_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map(|rows| rows.filter_map(|x| x.ok()).collect())
            .ok()
        })
        .unwrap_or_default();
    let mut resolved_sent = 0i64;
    let mut returned = 0i64;
    let mut still_unknown = 0i64;
    for (rid, customer_remote, email, subject) in unknowns {
        let Some(customer_remote) = customer_remote else {
            let _ = conn.execute(
                "UPDATE outreach_recipients SET state = 'failed',
                     last_error = 'Unknown outcome and no customer id to reconcile against'
                   WHERE id = ?1",
                [rid],
            );
            continue;
        };
        // Demo mode: the local mirror IS the remote (implementation
        // substitution). Look for a matching conversation.
        let match_conv: Option<(i64, Option<i64>)> = conn
            .query_row(
                "SELECT remote_id, number FROM conversations
                  WHERE customer_id = (SELECT id FROM customers WHERE remote_id = ?1)
                    AND LOWER(TRIM(COALESCE(subject, ''))) = LOWER(TRIM(COALESCE(?2, '')))
                  LIMIT 1",
                rusqlite::params![customer_remote, subject],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let attempts: i64 = conn
            .query_row(
                "SELECT attempts FROM outreach_recipients WHERE id = ?1",
                [rid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        match match_conv {
            Some((remote, number)) => {
                let _ = conn.execute(
                    "UPDATE outreach_recipients SET state = 'sent', hs_conversation_remote_id = ?1,
                         hs_conversation_number = ?2, sent_at = datetime('now')
                       WHERE id = ?3",
                    rusqlite::params![remote, number, rid],
                );
                log_event(
                    conn,
                    campaign_id,
                    Some(rid),
                    "recipient_reconciled_sent",
                    Some(&format!("found conversation {}", number.unwrap_or(remote))),
                );
                resolved_sent += 1;
            }
            None if attempts < MAX_ATTEMPTS => {
                let _ = conn.execute(
                    "UPDATE outreach_recipients SET state = 'failed',
                         last_error = 'Reconciled: not delivered, safe to retry'
                       WHERE id = ?1",
                    [rid],
                );
                log_event(
                    conn,
                    campaign_id,
                    Some(rid),
                    "recipient_reconciled_retry",
                    None,
                );
                returned += 1;
            }
            None => {
                log_event(
                    conn,
                    campaign_id,
                    Some(rid),
                    "recipient_reconciled_unknown",
                    Some("not found but attempts exhausted"),
                );
                still_unknown += 1;
            }
        }
        let _ = email;
    }
    let remaining = count_remaining(conn, campaign_id);
    if remaining > 0 {
        let _ = crate::jobs::enqueue_on(
            conn,
            "outreach",
            "outreach_send_batch",
            &serde_json::json!({ "campaignId": campaign_id }).to_string(),
            1,
            3,
        );
    } else {
        // v2.2.1 semantics: nothing left to send/reconcile -> terminal.
        if let Some(c) = get_outreach_campaign(conn, campaign_id).ok().flatten() {
            if c.status != "completed" && c.status != "cancelled" && c.status != "paused" {
                update_status(conn, campaign_id, "completed");
                let _ = conn.execute(
                    "UPDATE outreach_campaigns SET completed_at = datetime('now') WHERE id = ?1",
                    [campaign_id],
                );
                log_event(
                    conn,
                    campaign_id,
                    None,
                    "campaign_completed",
                    Some(&format!(
                        "{} sent, {} failed, {} skipped{}",
                        c.sent,
                        c.failed,
                        c.skipped,
                        if still_unknown > 0 {
                            format!(", {still_unknown} recipients left with unknown delivery outcomes (attempts exhausted)")
                        } else {
                            String::new()
                        }
                    )),
                );
            }
        }
    }
    serde_json::json!({ "resolvedSent": resolved_sent, "returnedToQueue": returned, "stillUnknown": still_unknown })
}

/// The not-found report shape (reference report() early return).
pub fn campaign_report_not_found() -> serde_json::Value {
    serde_json::json!({
        "campaign": serde_json::Value::Null,
        "totals": { "recipients": 0, "sent": 0, "failed": 0, "skipped": 0,
                    "cancelled": 0, "unknown": 0, "replied": 0, "reply_rate": serde_json::Value::Null },
        "replies": [],
        "note": "Campaign not found."
    })
}

/// `campaignService.report` (spec #52).
pub fn campaign_report(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    let Some(c) = get_outreach_campaign(conn, campaign_id).ok().flatten() else {
        return serde_json::json!({
            "campaign": serde_json::Value::Null,
            "totals": { "recipients": 0, "sent": 0, "failed": 0, "skipped": 0,
                        "cancelled": 0, "unknown": 0, "replied": 0, "reply_rate": serde_json::Value::Null },
            "replies": [],
            "note": "Campaign not found."
        });
    };
    let cancelled: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outreach_recipients WHERE campaign_id = ?1 AND state = 'cancelled'",
            [campaign_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let mut replies: Vec<serde_json::Value> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT r.customer_local_id, r.email, r.hs_conversation_number, r.replied_at,
                c.first_name, c.last_name, cv.id
           FROM outreach_recipients r
           LEFT JOIN customers c ON c.id = r.customer_local_id
           LEFT JOIN conversations cv ON cv.remote_id = r.hs_conversation_remote_id
          WHERE r.campaign_id = ?1 AND r.replied_at IS NOT NULL
          ORDER BY r.replied_at DESC LIMIT 500",
    ) {
        if let Ok(rows) = stmt.query_map([campaign_id], |r| {
            Ok(serde_json::json!({
                "customer_local_id": r.get::<_, i64>(0)?,
                "email": r.get::<_, Option<String>>(1)?,
                "hs_conversation_number": r.get::<_, Option<i64>>(2)?,
                "replied_at": r.get::<_, Option<String>>(3)?,
                "first_name": r.get::<_, Option<String>>(4)?,
                "last_name": r.get::<_, Option<String>>(5)?,
                "conversation_local_id": r.get::<_, Option<i64>>(6)?,
            }))
        }) {
            for row in rows.flatten() {
                let name = {
                    let first = row["first_name"].as_str().unwrap_or("");
                    let last = row["last_name"].as_str().unwrap_or("");
                    let joined = format!("{first} {last}").trim().to_string();
                    if !joined.is_empty() {
                        joined
                    } else if let Some(e) = row["email"].as_str() {
                        e.to_string()
                    } else {
                        format!("customer {}", row["customer_local_id"])
                    }
                };
                replies.push(serde_json::json!({
                    "customer": name,
                    "conversation_number": row["hs_conversation_number"],
                    "conversation_local_id": row["conversation_local_id"],
                    "replied_at": row["replied_at"],
                }));
            }
        }
    }
    let reply_rate = if c.sent > 0 {
        Some((c.replied as f64 / c.sent as f64 * 100.0).round() / 100.0)
    } else {
        None
    };
    serde_json::json!({
        "campaign": { "id": c.id, "name": c.name, "status": c.status },
        "totals": {
            "recipients": c.recipients, "sent": c.sent, "failed": c.failed,
            "skipped": c.skipped, "cancelled": cancelled, "unknown": c.unknown,
            "replied": c.replied, "reply_rate": reply_rate
        },
        "replies": replies,
        "note": "These are SupportOS campaign conversation outcomes based on Help Scout conversation state in the local mirror - not email-delivery analytics."
    })
}

/// `outreachRepo.deleteCampaign` — only when not queued/sending.
pub fn campaign_delete(conn: &Connection, campaign_id: i64) -> serde_json::Value {
    let Some(c) = get_outreach_campaign(conn, campaign_id).ok().flatten() else {
        return serde_json::json!({ "ok": false, "message": "Campaign not found." });
    };
    if c.status == "queued" || c.status == "sending" {
        return serde_json::json!({ "ok": false, "message": "Pause or cancel the campaign before deleting it." });
    }
    let ok = conn
        .execute(
            "DELETE FROM outreach_campaigns WHERE id = ?1",
            [campaign_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    serde_json::json!({ "ok": ok, "message": if ok { "Campaign deleted (audit events and Help Scout conversations are untouched." } else { "Delete failed." } })
}

// ─── Reference-shaped saved segments (segments table semantics) ────────────

/// Upgrade `saved_segments` to the reference `segments` shape in place:
/// description + condition_tree + version + updated_at columns
/// (idempotent; the legacy `criteria_json` column stays but is unused).
pub fn ensure_segments_v2(conn: &Connection) -> Result<()> {
    fn add_column_if_missing(
        conn: &Connection,
        table: &str,
        column: &str,
        decl: &str,
    ) -> Result<()> {
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info(?) WHERE name = ?",
                params![table, column],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if !exists {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
        }
        Ok(())
    }
    add_column_if_missing(conn, "saved_segments", "description", "TEXT")?;
    add_column_if_missing(conn, "saved_segments", "condition_tree", "TEXT")?;
    add_column_if_missing(
        conn,
        "saved_segments",
        "version",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    add_column_if_missing(
        conn,
        "saved_segments",
        "updated_at",
        "TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )?;
    Ok(())
}

fn segment_row_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<serde_json::Value> {
    let id: i64 = r.get(0)?;
    let name: String = r.get(1)?;
    let description: Option<String> = r.get(2)?;
    let condition_tree: Option<String> = r.get(3)?;
    let version: i64 = r.get::<_, Option<i64>>(4)?.unwrap_or(1);
    let created_at: String = r.get(5)?;
    let updated_at: Option<String> = r.get(6)?;
    // parseTree: any stored JSON with conditions[]+exclude[] arrays, else the
    // empty tree (never fails the request).
    let definition = condition_tree
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .filter(|v| {
            v.get("conditions").and_then(|c| c.as_array()).is_some()
                && v.get("exclude").and_then(|e| e.as_array()).is_some()
        })
        .unwrap_or_else(
            || serde_json::json!({"combinator": "all", "conditions": [], "exclude": []}),
        );
    Ok(serde_json::json!({
        "id": id,
        "name": name,
        "description": description,
        "definition": definition,
        "version": version,
        "created_at": created_at,
        "updated_at": updated_at.unwrap_or(created_at),
    }))
}

/// `outreachRepo.listSegments` — reference shape, newest update first.
pub fn list_segments_v2(conn: &Connection) -> Result<Vec<serde_json::Value>> {
    ensure_segments_v2(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, description, condition_tree, version, created_at, updated_at
           FROM saved_segments ORDER BY updated_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], segment_row_json)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.into())
}

/// `outreachRepo.getSegment` — the parsed definition for campaign creation.
pub fn get_segment_v2(conn: &Connection, id: i64) -> Result<Option<serde_json::Value>> {
    ensure_segments_v2(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, description, condition_tree, version, created_at, updated_at
           FROM saved_segments WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map([id], segment_row_json)?;
    Ok(rows.next().transpose()?)
}

/// `outreachRepo.saveSegment` — insert, or update with version increment.
pub fn save_segment_v2(
    conn: &Connection,
    id: Option<i64>,
    name: &str,
    description: Option<&str>,
    definition_json: &str,
) -> Result<i64> {
    ensure_segments_v2(conn)?;
    if let Some(id) = id.filter(|id| *id > 0) {
        let exists: bool = conn
            .query_row("SELECT 1 FROM saved_segments WHERE id = ?1", [id], |_| {
                Ok(true)
            })
            .unwrap_or(false);
        if exists {
            conn.execute(
                "UPDATE saved_segments SET name = ?1, description = ?2, condition_tree = ?3,
                        version = version + 1, updated_at = datetime('now')
                  WHERE id = ?4",
                params![name, description, definition_json, id],
            )?;
            return Ok(id);
        }
    }
    conn.execute(
        "INSERT INTO saved_segments (name, description, condition_tree, version)
         VALUES (?1, ?2, ?3, 1)",
        params![name, description, definition_json],
    )?;
    Ok(conn.last_insert_rowid())
}

// ─── Reference-shaped campaign creation + listing ──────────────────────────

/// `outreachRepo.createCampaign` — static recipient snapshot with the
/// why-selected evidence (spec #33/#34/#17).
#[allow(clippy::too_many_arguments)]
pub fn create_outreach_campaign(
    conn: &Connection,
    name: &str,
    subject: &str,
    body: &str,
    mailbox_local_id: i64,
    tags: &[String],
    segment_id: Option<i64>,
    segment_snapshot: Option<&str>,
    recipients: &[serde_json::Value],
) -> Result<i64> {
    conn.execute(
        "INSERT INTO outreach_campaigns (name, subject, body, mailbox_local_id, tags, status, segment_id, segment_snapshot)
         VALUES (?1, ?2, ?3, ?4, ?5, 'draft', ?6, ?7)",
        params![name, subject, body, mailbox_local_id, serde_json::to_string(tags).unwrap_or_default(), segment_id, segment_snapshot],
    )?;
    let campaign_id = conn.last_insert_rowid();
    let mut ins = conn.prepare(
        "INSERT OR IGNORE INTO outreach_recipients
             (campaign_id, customer_local_id, customer_remote_id, email, snapshot)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for rec in recipients {
        let snapshot = serde_json::json!({
            "why": rec.get("why").cloned().unwrap_or(serde_json::json!([])),
            "matching_tickets": rec.get("matching_tickets").cloned().unwrap_or(serde_json::json!([])),
            "property_values": rec.get("properties").cloned().unwrap_or(serde_json::json!([])),
            "selected_at": now_iso(),
        });
        ins.execute(params![
            campaign_id,
            rec.get("customer_local_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            rec.get("customer_remote_id").and_then(|v| v.as_i64()),
            rec.get("chosen_email").and_then(|v| v.as_str()),
            serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string()),
        ])?;
    }
    log_event(
        conn,
        campaign_id,
        None,
        "campaign_created",
        Some(&format!("{} recipients snapshotted", recipients.len())),
    );
    Ok(campaign_id)
}

/// nowIso (repos/helpers.ts).
fn now_iso() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            let secs = d.as_secs();
            let days = secs / 86400;
            let rem = secs % 86400;
            let (y, mo, da) = civil_from_days(days as i64);
            format!(
                "{y:04}-{mo:02}-{da:02}T{:02}:{:02}:{:02}.000Z",
                rem / 3600,
                (rem % 3600) / 60,
                rem % 60
            )
        })
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_string())
}

/// days-since-epoch → civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `outreachRepo.listCampaigns` — the CAMPAIGN_SELECT rows, newest first.
pub fn list_outreach_campaigns(conn: &Connection) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(&format!(
        "{CAMPAIGN_SELECT} ORDER BY oc.created_at DESC, oc.id DESC"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "name": r.get::<_, String>(1)?,
            "subject": r.get::<_, String>(2)?,
            "status": r.get::<_, String>(6)?,
            "mailbox_local_id": r.get::<_, Option<i64>>(4)?,
            "mailbox_name": r.get::<_, Option<String>>(11)?,
            "segment_id": r.get::<_, Option<i64>>(7)?,
            "segment_name": r.get::<_, Option<String>>(12)?,
            "recipients": r.get::<_, i64>(13)?,
            "sent": r.get::<_, i64>(14)?,
            "failed": r.get::<_, i64>(15)?,
            "skipped": r.get::<_, i64>(16)?,
            "unknown": r.get::<_, i64>(17)?,
            "replied": r.get::<_, i64>(18)?,
            "created_at": r.get::<_, String>(8)?,
            "queued_at": r.get::<_, Option<String>>(9)?,
            "completed_at": r.get::<_, Option<String>>(10)?,
        }))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.into())
}

/// `outreachRepo.listRecipients` — snapshot-backed evidence rows.
pub fn list_campaign_recipients(
    conn: &Connection,
    campaign_id: i64,
    limit: i64,
) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.customer_local_id, r.customer_remote_id, r.email, r.snapshot,
                r.state, r.attempts, r.last_error, r.hs_conversation_remote_id,
                r.hs_conversation_number, r.sent_at, r.replied_at,
                c.first_name, c.last_name
           FROM outreach_recipients r
           LEFT JOIN customers c ON c.id = r.customer_local_id
          WHERE r.campaign_id = ?1 ORDER BY r.id LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![campaign_id, limit], |r| {
        let snapshot_raw: String = r.get(4)?;
        let snapshot: serde_json::Value =
            serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "customer_local_id": r.get::<_, i64>(1)?,
            "customer_remote_id": r.get::<_, Option<i64>>(2)?,
            "first_name": r.get::<_, Option<String>>(12)?,
            "last_name": r.get::<_, Option<String>>(13)?,
            "email": r.get::<_, Option<String>>(3)?,
            "state": r.get::<_, String>(5)?,
            "attempts": r.get::<_, i64>(6)?,
            "last_error": r.get::<_, Option<String>>(7)?,
            "hs_conversation_remote_id": r.get::<_, Option<i64>>(8)?,
            "hs_conversation_number": r.get::<_, Option<i64>>(9)?,
            "conversation_local_id": serde_json::Value::Null,
            "sent_at": r.get::<_, Option<String>>(10)?,
            "replied_at": r.get::<_, Option<String>>(11)?,
            "why": snapshot.get("why").cloned().unwrap_or(serde_json::json!([])),
            "matching_tickets": snapshot.get("matching_tickets").cloned().unwrap_or(serde_json::json!([])),
        }))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.into())
}

/// `outreachRepo.getCampaign` — CampaignDetail + tags + snapshot + list.
pub fn get_campaign_full(conn: &Connection, id: i64) -> Result<Option<serde_json::Value>> {
    let Some(c) = get_outreach_campaign(conn, id)? else {
        return Ok(None);
    };
    let segment_snapshot: serde_json::Value = conn
        .query_row(
            "SELECT segment_snapshot FROM outreach_campaigns WHERE id = ?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter(|v: &serde_json::Value| {
            v.get("conditions").and_then(|x| x.as_array()).is_some()
                && v.get("exclude").and_then(|x| x.as_array()).is_some()
        })
        .unwrap_or_else(
            || serde_json::json!({"combinator": "all", "conditions": [], "exclude": []}),
        );
    let recipients_list = list_campaign_recipients(conn, id, 1000)?;
    Ok(Some(serde_json::json!({
        "id": c.id,
        "name": c.name,
        "subject": c.subject,
        "body": c.body,
        "status": c.status,
        "mailbox_local_id": c.mailbox_local_id,
        "mailbox_name": c.mailbox_name,
        "segment_id": c.segment_id,
        "segment_name": c.segment_name,
        "tags": c.tags,
        "segment_snapshot": segment_snapshot,
        "recipients": c.recipients,
        "sent": c.sent,
        "failed": c.failed,
        "skipped": c.skipped,
        "unknown": c.unknown,
        "replied": c.replied,
        "created_at": c.created_at,
        "queued_at": c.queued_at,
        "completed_at": c.completed_at,
        "recipients_list": recipients_list,
    })))
}

// ===========================================================================
// Campaign send executor (audit OR-02 / B3): batch 5, attempts 3,
// provider.createConversation, sync-back, unknown-state reconcile.
//
// The queue/resume/retry/reconcile services all enqueue `outreach_send_batch`
// jobs; the worker (workers.rs) dispatches each to `send_batch` below. The
// executor never holds the DB mutex across the provider call — the batch is
// fetched under a short lock, the lock is dropped for the network call, and a
// fresh lock re-acquires for the per-recipient write-back (mirrors the
// sync_conversation_ratings pattern in workers.rs and avoids the M28 global
// mutex freeze that would otherwise let one slow send block all requests).
// ===========================================================================

/// The maximum number of recipients to attempt per batch tick (spec #27).
pub const SEND_BATCH_SIZE: i64 = 5;

/// Per-recipient retry budget (spec #31; KNOWN PITFALLS: "recipients that
/// exhaust retries must fail, never livelock"). Hard cap; not configurable
/// per-campaign in the port today (the reference shares the same constant).
pub const SEND_MAX_ATTEMPTS: i64 = 3;

/// One row fetched for the batch — every field the executor needs to make a
/// send decision and persist the outcome without re-querying.
#[allow(dead_code)]
struct BatchRow {
    recipient_id: i64,
    customer_local_id: i64,
    customer_remote_id: Option<i64>,
    email: Option<String>,
    snapshot: String,
    attempts: i64,
}

/// The send outcome for one recipient — produced by the provider call (or
/// the pre-send skip filter). The executor uses this to write the per-row
/// `outreach_attempts` row + the `outreach_events` log entry, and to drive
/// the recipient state machine.
enum SendOutcome {
    Sent { remote_id: i64, number: i64 },
    FailedPermanent(String),
    FailedRetryable(String),
    Unknown(String),
}

impl SendOutcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Sent { .. } => "sent",
            Self::FailedPermanent(_) => "failed",
            Self::FailedRetryable(_) => "queued",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// Pre-send skip filter: DNC + missing/invalid email → `skipped` (so the
/// campaign counters reflect them) and not selected for sending. Returns the
/// number of recipients parked at `skipped` by this call.
fn skip_ineligible_recipients(conn: &Connection, campaign_id: i64) -> i64 {
    // No usable email.
    let no_email = conn
        .execute(
            "UPDATE outreach_recipients
                SET state = 'skipped', last_error = 'No usable email address on file.'
              WHERE campaign_id = ?1
                AND state IN ('selected','queued')
                AND (email IS NULL OR TRIM(email) = '')",
            [campaign_id],
        )
        .unwrap_or(0) as i64;
    // DNC list members.
    let on_dnc = conn
        .execute(
            "UPDATE outreach_recipients
                SET state = 'skipped', last_error = 'On Do-Not-Contact list.'
              WHERE campaign_id = ?1
                AND state IN ('selected','queued')
                AND customer_local_id IN (SELECT customer_id FROM do_not_contact)",
            [campaign_id],
        )
        .unwrap_or(0) as i64;
    no_email + on_dnc
}

/// Pick the next up-to-5 recipients that are ready to send.
fn pick_batch(conn: &Connection, campaign_id: i64) -> Vec<BatchRow> {
    let mut stmt = match conn.prepare(
        "SELECT id, customer_local_id, customer_remote_id, email, snapshot, attempts
           FROM outreach_recipients
          WHERE campaign_id = ?1 AND state IN ('selected','queued')
          ORDER BY id ASC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt
        .query_map(params![campaign_id, SEND_BATCH_SIZE], |r| {
            Ok(BatchRow {
                recipient_id: r.get(0)?,
                customer_local_id: r.get(1)?,
                customer_remote_id: r.get(2)?,
                email: r.get(3)?,
                snapshot: r.get(4)?,
                attempts: r.get(5)?,
            })
        })
        .ok();
    rows.map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Classify a provider error into permanent / retryable / unknown.
///
/// - 4xx that are the caller's fault (400/403/404/409/412/413/415/423) →
///   `FailedPermanent`: another attempt will not help.
/// - 429 + 5xx (500/502/503/504) → `FailedRetryable`: the next batch will
///   retry; if `attempts` has reached `SEND_MAX_ATTEMPTS` the caller marks
///   the recipient `failed`.
/// - 2xx-but-no-id (the `200/unknown-id` sentinel) → `Unknown`: parked for
///   the reconcile pass.
/// - Network errors / status 0 → `FailedRetryable` (one more attempt later).
fn classify_error(e: &crate::error::Error, attempts: i64) -> SendOutcome {
    let status = crate::helpscout_real::hs_status(e).unwrap_or(0);
    let label = e.to_string();
    match status {
        0 => SendOutcome::FailedRetryable(format!("Network error: {label}")),
        200 => SendOutcome::Unknown(format!(
            "Provider accepted the send but returned no conversation id. {label}"
        )),
        400 | 403 | 404 | 409 | 412 | 413 | 415 | 423 => {
            SendOutcome::FailedPermanent(format!("Provider error {status}: {label}"))
        }
        429 | 500 | 502 | 503 | 504 => {
            if attempts + 1 >= SEND_MAX_ATTEMPTS {
                SendOutcome::FailedPermanent(format!(
                    "Provider error {status} after {attempts} attempts: {label}"
                ))
            } else {
                SendOutcome::FailedRetryable(format!("Provider error {status}: {label}"))
            }
        }
        _ => SendOutcome::FailedRetryable(format!("Provider error {status}: {label}")),
    }
}

/// Mark a recipient's transition state and write the per-attempt row +
/// outreach_events log. The caller passes the new state explicitly so the
/// same helper serves the sent / failed-permanent / retryable / unknown
/// paths.
fn record_outcome(
    conn: &Connection,
    campaign_id: i64,
    row: &BatchRow,
    attempt_no: i64,
    outcome: &SendOutcome,
    error_or_remote: Option<&str>,
) {
    let now = datetime_now();
    // outreach_attempts: one row per attempt.
    let _ = conn.execute(
        "INSERT INTO outreach_attempts
             (recipient_id, attempt_no, started_at, finished_at, result, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            row.recipient_id,
            attempt_no,
            now,
            datetime_now(),
            outcome.label(),
            match outcome {
                SendOutcome::Sent { .. } => None,
                SendOutcome::FailedPermanent(m)
                | SendOutcome::FailedRetryable(m)
                | SendOutcome::Unknown(m) => Some(m.as_str()),
            }
        ],
    );

    // State transition for the recipient.
    let _ = match outcome {
        SendOutcome::Sent { remote_id, number } => conn.execute(
            "UPDATE outreach_recipients
                SET state = 'sent',
                    hs_conversation_remote_id = ?1,
                    hs_conversation_number = ?2,
                    sent_at = datetime('now'),
                    last_error = NULL
              WHERE id = ?3",
            params![remote_id, number, row.recipient_id],
        ),
        SendOutcome::FailedPermanent(msg) => conn.execute(
            "UPDATE outreach_recipients
                SET state = 'failed', last_error = ?1
              WHERE id = ?2",
            params![msg, row.recipient_id],
        ),
        SendOutcome::FailedRetryable(msg) => conn.execute(
            "UPDATE outreach_recipients
                SET state = 'queued', last_error = ?1
              WHERE id = ?2",
            params![msg, row.recipient_id],
        ),
        SendOutcome::Unknown(msg) => conn.execute(
            "UPDATE outreach_recipients
                SET state = 'unknown', last_error = ?1
              WHERE id = ?2",
            params![msg, row.recipient_id],
        ),
    };

    // outreach_events: the audit-grade log of what happened.
    let event = match outcome {
        SendOutcome::Sent { .. } => "recipient_sent",
        SendOutcome::FailedPermanent(_) => "recipient_failed",
        SendOutcome::FailedRetryable(_) => "recipient_retryable_error",
        SendOutcome::Unknown(_) => "recipient_unknown",
    };
    log_event(
        conn,
        campaign_id,
        Some(row.recipient_id),
        event,
        error_or_remote,
    );
}

/// `datetime('now')` in SQLite produces `YYYY-MM-DD HH:MM:SS`; the
/// reference's `outreach_events.at` column uses the same shape.
fn datetime_now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// The send executor (audit OR-02 / B3).
///
/// Workflow per call:
/// 1. Skip-eligible filter: park DNC + no-email recipients at `skipped`.
/// 2. Pick up to 5 recipients in `selected`/`queued` state.
/// 3. For each recipient: render the template, call
///    `provider.create_conversation`, classify the outcome, write
///    `outreach_attempts` + `outreach_events` + recipient state.
/// 4. If any recipient succeeded: enqueue `sync_conversation` jobs (the
///    sync-back) so the new conversation lands in the local mirror.
/// 5. If more recipients remain in `selected`/`queued`/`sending`: enqueue
///    the next `outreach_send_batch` job (the campaign stays in `sending`).
/// 6. Otherwise (nothing left to send): mark the campaign `completed`.
///
/// The function takes the provider as an `Arc<dyn HelpScoutProvider>` so the
/// worker can pass its own `self.provider` directly. The DB lock is only
/// held for the fetch + per-recipient write — never across the provider call
/// (mirrors `sync_conversation_ratings` in workers.rs).
pub async fn send_batch(
    conn: &Arc<Mutex<Connection>>,
    provider: &Arc<dyn crate::helpscout::HelpScoutProvider>,
    campaign_id: i64,
) -> serde_json::Value {
    // 1. Load the campaign + recipients under a short lock, then drop the
    //    lock before any provider call.
    let (campaign, mailbox_local_id, tags, subject, body, batch) = {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = skip_ineligible_recipients(&c, campaign_id);
        let Some(camp) = get_outreach_campaign(&c, campaign_id).unwrap_or_default() else {
            return serde_json::json!({ "ok": false, "message": "Campaign not found." });
        };
        if camp.status == "paused" || camp.status == "cancelled" || camp.status == "completed" {
            return serde_json::json!({
                "ok": false,
                "message": format!(
                    "Campaign is {}; nothing to send.",
                    camp.status
                ),
            });
        }
        // Mark the campaign as actively sending (idempotent).
        if camp.status != "sending" {
            update_status(&c, campaign_id, "sending");
        }
        let mailbox_local_id = camp.mailbox_local_id;
        let tags = camp.tags.clone();
        let subject = camp.subject.clone();
        let body = camp.body.clone();
        let batch = pick_batch(&c, campaign_id);
        (camp, mailbox_local_id, tags, subject, body, batch)
    };

    let _ = campaign; // Camp is here for the early-return shape check above.

    if batch.is_empty() {
        // Nothing to send this tick — finalize if no work remains at all.
        return finalize_if_drained(conn, campaign_id);
    }

    let Some(mailbox_local_id) = mailbox_local_id else {
        // No mailbox configured — every remaining recipient is failed.
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        for row in &batch {
            record_outcome(
                &c,
                campaign_id,
                row,
                row.attempts + 1,
                &SendOutcome::FailedPermanent("Campaign has no sending mailbox configured.".into()),
                Some("no mailbox"),
            );
        }
        return finalize_if_drained(conn, campaign_id);
    };

    let mut sent_remote_ids: Vec<i64> = Vec::new();
    let mut counts = BatchCounts::default();

    for row in batch {
        let attempt_no = row.attempts + 1;

        // The render context (snapshot carries the matching_tickets for
        // personalization). Customer missing → fail permanently.
        let Some(customer_remote_id) = row.customer_remote_id else {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            record_outcome(
                &c,
                campaign_id,
                &row,
                attempt_no,
                &SendOutcome::FailedPermanent(
                    "Recipient has no remote customer id; cannot create conversation.".into(),
                ),
                Some("no customer_remote_id"),
            );
            counts.failed_permanent += 1;
            continue;
        };

        // Render the personalized subject/body under a brief lock (the
        // render_for helper queries customer + organization names).
        let rendered = {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            let matching_tickets: Vec<serde_json::Value> =
                serde_json::from_str::<serde_json::Value>(&row.snapshot)
                    .ok()
                    .and_then(|v| v.get("matching_tickets").cloned())
                    .and_then(|t| serde_json::from_value(t).ok())
                    .unwrap_or_default();
            render_for(
                &c,
                row.customer_local_id,
                &matching_tickets,
                &subject,
                &body,
            )
        };
        let r_subject = rendered["subject"].as_str().unwrap_or(&subject).to_string();
        let r_body = rendered["body"].as_str().unwrap_or(&body).to_string();

        // Provider call — no DB lock held.
        let input = crate::helpscout::CreateConversationInput {
            mailbox_id: mailbox_local_id,
            customer_id: customer_remote_id,
            subject: r_subject,
            body: r_body,
            tags: tags.clone(),
            status: Some("active".to_string()),
        };
        let result = provider.create_conversation(input).await;

        let outcome = match result {
            Ok(crate::helpscout::ConversationCreated {
                conversation_id,
                number,
                ..
            }) => {
                counts.sent += 1;
                sent_remote_ids.push(conversation_id);
                SendOutcome::Sent {
                    remote_id: conversation_id,
                    number,
                }
            }
            Err(e) => {
                let mut o = classify_error(&e, row.attempts);
                match &o {
                    SendOutcome::Sent { .. } => {}
                    SendOutcome::FailedPermanent(_) => counts.failed_permanent += 1,
                    SendOutcome::FailedRetryable(_) => counts.retryable += 1,
                    SendOutcome::Unknown(_) => counts.unknown += 1,
                }
                // Downgrade retryable to permanent when the attempt budget
                // is exhausted — KNOWN PITFALLS: "recipients that exhaust
                // retries must fail, never livelock."
                if let SendOutcome::FailedRetryable(msg) = &o {
                    if attempt_no >= SEND_MAX_ATTEMPTS {
                        counts.retryable -= 1;
                        counts.failed_permanent += 1;
                        o = SendOutcome::FailedPermanent(format!(
                            "{msg} (attempts exhausted: {attempt_no})"
                        ));
                    }
                }
                o
            }
        };

        let error_or_remote: Option<&str> = match &outcome {
            SendOutcome::Sent { remote_id, .. } => {
                // Persist immediately while the result is fresh.
                let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                record_outcome(
                    &c,
                    campaign_id,
                    &row,
                    attempt_no,
                    &outcome,
                    Some(&remote_id.to_string()),
                );
                None
            }
            SendOutcome::FailedPermanent(m)
            | SendOutcome::FailedRetryable(m)
            | SendOutcome::Unknown(m) => {
                let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                record_outcome(&c, campaign_id, &row, attempt_no, &outcome, Some(m));
                None
            }
        };
        let _ = error_or_remote;
    }

    // 4. Sync-back: enqueue one sync_conversation job per newly-created
    //    conversation so the local mirror catches up. This is the
    //    "sync-back" leg of the audit's spec for OR-02.
    if !sent_remote_ids.is_empty() {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        for rid in &sent_remote_ids {
            let _ = crate::jobs::enqueue_on(
                &c,
                "sync",
                "sync_conversation",
                &serde_json::json!({ "remoteId": rid }).to_string(),
                2,
                2,
            );
        }
        log_event(
            &c,
            campaign_id,
            None,
            "batch_sync_back_enqueued",
            Some(&format!("{} sync_conversation jobs", sent_remote_ids.len())),
        );
    }

    // Surface the per-batch outcome counts so the job log shows progress
    // (and the field is "read" — keeps the warnings honest).
    tracing::info!(
        campaign_id = campaign_id,
        sent = counts.sent,
        failed = counts.failed_permanent,
        retryable = counts.retryable,
        unknown = counts.unknown,
        "Outreach batch outcomes"
    );

    // 5/6. Re-enqueue next batch or finalize.
    let mut summary = finalize_if_drained(conn, campaign_id);
    if let Some(obj) = summary.as_object_mut() {
        obj.insert(
            "batch".to_string(),
            serde_json::json!({
                "sent": counts.sent,
                "failed": counts.failed_permanent,
                "retryable": counts.retryable,
                "unknown": counts.unknown,
            }),
        );
    }
    summary
}

#[derive(Default)]
struct BatchCounts {
    sent: i64,
    failed_permanent: i64,
    retryable: i64,
    unknown: i64,
}

/// Count recipients still eligible to send (selected/queued/sending).
fn count_remaining_for_send(conn: &Connection, campaign_id: i64) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM outreach_recipients
          WHERE campaign_id = ?1 AND state IN ('selected','queued','sending')",
        [campaign_id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// After a batch finishes: if recipients still remain in a sendable state,
/// enqueue the next `outreach_send_batch` job; otherwise flip the campaign
/// to `completed` and log a final `campaign_completed` event. Returns the
/// JSON summary the worker can attach to the job log.
fn finalize_if_drained(conn: &Arc<Mutex<Connection>>, campaign_id: i64) -> serde_json::Value {
    let (remaining, sent, failed, skipped, unknown, status) = {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        let remaining = count_remaining_for_send(&c, campaign_id);
        let camp = get_outreach_campaign(&c, campaign_id)
            .ok()
            .flatten()
            .map(|c| (c.sent, c.failed, c.skipped, c.unknown, c.status));
        let (sent, failed, skipped, unknown, status) =
            camp.unwrap_or((0, 0, 0, 0, "unknown".into()));
        (remaining, sent, failed, skipped, unknown, status)
    };

    if remaining > 0 {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = crate::jobs::enqueue_on(
            &c,
            "outreach",
            "outreach_send_batch",
            &serde_json::json!({ "campaignId": campaign_id }).to_string(),
            1,
            3,
        );
        return serde_json::json!({
            "ok": true,
            "campaign_id": campaign_id,
            "remaining": remaining,
            "message": "Batch processed; next batch enqueued.",
        });
    }

    // Drain complete — flip the campaign to `completed` (idempotent; a
    // cancelled/paused campaign stays as-is).
    {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        if status != "completed" && status != "cancelled" && status != "paused" {
            update_status(&c, campaign_id, "completed");
            let _ = c.execute(
                "UPDATE outreach_campaigns SET completed_at = datetime('now') WHERE id = ?1",
                [campaign_id],
            );
            log_event(
                &c,
                campaign_id,
                None,
                "campaign_completed",
                Some(&format!(
                    "{sent} sent, {failed} failed, {skipped} skipped, {unknown} unknown"
                )),
            );
        }
    }

    serde_json::json!({
        "ok": true,
        "campaign_id": campaign_id,
        "remaining": 0,
        "message": "Campaign completed: all recipients processed.",
        "totals": {
            "sent": sent, "failed": failed, "skipped": skipped, "unknown": unknown
        }
    })
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
        apply_m023_to_m025(&conn).unwrap();
        conn
    }

    // ---- M023–M025 migrations ----------------------------------------------

    #[test]
    fn m023_creates_saved_segments() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM saved_segments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m024_creates_campaigns() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM campaigns", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m025_creates_do_not_contact() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM do_not_contact", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m023_to_m025_is_idempotent() {
        let conn = fresh_db();
        apply_m023_to_m025(&conn).unwrap();
    }

    // ---- M9-T01: Saved segments ---------------------------------------------

    #[test]
    fn create_segment_round_trips() {
        let conn = fresh_db();
        let id = create_segment(&conn, "VIP customers", r#"{"tags":["vip"]}"#).unwrap();
        assert!(id > 0);
        let seg = get_segment(&conn, id).unwrap().unwrap();
        assert_eq!(seg.name, "VIP customers");
        assert_eq!(seg.criteria, r#"{"tags":["vip"]}"#);
    }

    #[test]
    fn list_segments_returns_all() {
        let conn = fresh_db();
        create_segment(&conn, "A", "{}").unwrap();
        create_segment(&conn, "B", "{}").unwrap();
        assert_eq!(list_segments(&conn).unwrap().len(), 2);
    }

    #[test]
    fn delete_segment_works() {
        let conn = fresh_db();
        let id = create_segment(&conn, "X", "{}").unwrap();
        assert!(delete_segment(&conn, id).unwrap());
        assert!(get_segment(&conn, id).unwrap().is_none());
    }

    #[test]
    fn get_segment_returns_none_for_nonexistent() {
        let conn = fresh_db();
        assert!(get_segment(&conn, 9999).unwrap().is_none());
    }

    // ---- M9-T02: Campaigns --------------------------------------------------

    #[test]
    fn campaign_lifecycle() {
        let conn = fresh_db();
        let seg_id = create_segment(&conn, "All", "{}").unwrap();
        let camp_id =
            create_campaign(&conn, "Spring outreach", Some(seg_id), Some("Hello!")).unwrap();

        // Add recipients.
        add_campaign_recipient(&conn, camp_id, 2001).unwrap();
        add_campaign_recipient(&conn, camp_id, 2002).unwrap();

        // Start.
        assert!(start_campaign(&conn, camp_id).unwrap());

        // Can't start again.
        assert!(!start_campaign(&conn, camp_id).unwrap(), "already sending");

        // Mark one sent.
        let recipient_id: i64 = conn
            .query_row(
                "SELECT id FROM campaign_recipients WHERE campaign_id = ?1 AND customer_id = 2001",
                params![camp_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(mark_recipient_sent(&conn, recipient_id).unwrap());

        // Count active.
        assert_eq!(
            count_active_campaigns(&conn).unwrap(),
            1,
            "one campaign sending"
        );
    }

    #[test]
    fn recipient_exhausted_after_max_attempts() {
        let conn = fresh_db();
        let camp_id = create_campaign(&conn, "C", None, None).unwrap();
        add_campaign_recipient(&conn, camp_id, 2001).unwrap();
        let recipient_id: i64 = conn
            .query_row(
                "SELECT id FROM campaign_recipients WHERE campaign_id = ?1",
                params![camp_id],
                |r| r.get(0),
            )
            .unwrap();

        // Default max = 3.
        increment_attempt(&conn, recipient_id).unwrap();
        assert!(!recipient_exhausted(&conn, recipient_id).unwrap());
        increment_attempt(&conn, recipient_id).unwrap();
        assert!(!recipient_exhausted(&conn, recipient_id).unwrap());
        increment_attempt(&conn, recipient_id).unwrap();
        assert!(
            recipient_exhausted(&conn, recipient_id).unwrap(),
            "exhausted after 3 attempts"
        );
    }

    #[test]
    fn retry_failed_resets_attempt_budget() {
        let conn = fresh_db();
        let camp_id = create_campaign(&conn, "C", None, None).unwrap();
        add_campaign_recipient(&conn, camp_id, 2001).unwrap();
        let recipient_id: i64 = conn
            .query_row(
                "SELECT id FROM campaign_recipients WHERE campaign_id = ?1",
                params![camp_id],
                |r| r.get(0),
            )
            .unwrap();

        // Exhaust + fail.
        for _ in 0..3 {
            increment_attempt(&conn, recipient_id).unwrap();
        }
        assert!(recipient_exhausted(&conn, recipient_id).unwrap());
        mark_recipient_failed(&conn, recipient_id, "timeout").unwrap();

        // Retry failed — resets budget.
        let reset_count = retry_failed_recipients(&conn, camp_id).unwrap();
        assert_eq!(reset_count, 1, "one recipient reset");

        // No longer exhausted.
        assert!(
            !recipient_exhausted(&conn, recipient_id).unwrap(),
            "budget reset → not exhausted"
        );

        // Status is pending again.
        let status: String = conn
            .query_row(
                "SELECT status FROM campaign_recipients WHERE id = ?1",
                params![recipient_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "pending");
    }

    #[test]
    fn mark_recipient_failed_stores_error() {
        let conn = fresh_db();
        let camp_id = create_campaign(&conn, "C", None, None).unwrap();
        add_campaign_recipient(&conn, camp_id, 2001).unwrap();
        let rid: i64 = conn
            .query_row(
                "SELECT id FROM campaign_recipients WHERE campaign_id = ?1",
                params![camp_id],
                |r| r.get(0),
            )
            .unwrap();
        mark_recipient_failed(&conn, rid, "SMTP timeout").unwrap();

        let (status, error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, last_error FROM campaign_recipients WHERE id = ?1",
                params![rid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(error.as_deref(), Some("SMTP timeout"));
    }

    // ---- M9-T03: Do-not-contact --------------------------------------------

    #[test]
    fn dnc_add_check_remove() {
        let conn = fresh_db();
        assert!(!is_on_dnc(&conn, 2001).unwrap());
        add_to_dnc(&conn, 2001, "unsubscribed").unwrap();
        assert!(is_on_dnc(&conn, 2001).unwrap());
        assert!(remove_from_dnc(&conn, 2001).unwrap());
        assert!(!is_on_dnc(&conn, 2001).unwrap());
    }

    #[test]
    fn dnc_list() {
        let conn = fresh_db();
        add_to_dnc(&conn, 2001, "bounce").unwrap();
        add_to_dnc(&conn, 2002, "complaint").unwrap();
        let list = list_dnc(&conn).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn dnc_add_is_idempotent() {
        let conn = fresh_db();
        add_to_dnc(&conn, 2001, "manual").unwrap();
        add_to_dnc(&conn, 2001, "updated_reason").unwrap(); // upsert
        let list = list_dnc(&conn).unwrap();
        assert_eq!(list.len(), 1, "no duplicate DNC entries");
        assert_eq!(list[0].1, "updated_reason", "reason updated");
    }

    #[test]
    fn remove_nonexistent_dnc_returns_false() {
        let conn = fresh_db();
        assert!(!remove_from_dnc(&conn, 9999).unwrap());
    }

    // ---- M9-T04: Campaign monitoring ---------------------------------------

    #[test]
    fn campaign_stats_empty() {
        let conn = fresh_db();
        let camp_id = create_campaign(&conn, "C", None, None).unwrap();
        let stats = get_campaign_stats(&conn, camp_id).unwrap();
        assert_eq!(stats.total_recipients, 0);
        assert_eq!(stats.reply_rate, 0.0);
    }

    #[test]
    fn campaign_stats_with_data() {
        let conn = fresh_db();
        let camp_id = create_campaign(&conn, "C", None, None).unwrap();
        add_campaign_recipient(&conn, camp_id, 2001).unwrap();
        add_campaign_recipient(&conn, camp_id, 2002).unwrap();
        add_campaign_recipient(&conn, camp_id, 2003).unwrap();

        // Mark 2 sent, 1 failed.
        let r1: i64 = conn
            .query_row(
                "SELECT id FROM campaign_recipients WHERE customer_id = 2001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let r2: i64 = conn
            .query_row(
                "SELECT id FROM campaign_recipients WHERE customer_id = 2002",
                [],
                |r| r.get(0),
            )
            .unwrap();
        mark_recipient_sent(&conn, r1).unwrap();
        mark_recipient_sent(&conn, r2).unwrap();

        // Mark r1 as replied.
        mark_recipient_replied(&conn, r1).unwrap();

        let stats = get_campaign_stats(&conn, camp_id).unwrap();
        assert_eq!(stats.total_recipients, 3);
        assert_eq!(stats.sent, 2);
        assert_eq!(stats.replied, 1);
        assert!(
            (stats.reply_rate - 0.5).abs() < 1e-6,
            "1 reply / 2 sent = 0.5"
        );
    }

    #[test]
    fn mark_recipient_replied_is_idempotent() {
        let conn = fresh_db();
        let camp_id = create_campaign(&conn, "C", None, None).unwrap();
        add_campaign_recipient(&conn, camp_id, 2001).unwrap();
        let rid: i64 = conn
            .query_row(
                "SELECT id FROM campaign_recipients WHERE campaign_id = ?1",
                params![camp_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(mark_recipient_replied(&conn, rid).unwrap());
        assert!(
            !mark_recipient_replied(&conn, rid).unwrap(),
            "second call returns false"
        );
    }

    // ---- serde --------------------------------------------------------------

    #[test]
    fn saved_segment_serializes() {
        let s = SavedSegment {
            id: Some(1),
            name: "VIP".into(),
            criteria: "{}".into(),
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"name\":\"VIP\""));
    }

    #[test]
    fn campaign_serializes() {
        let c = Campaign {
            id: Some(1),
            name: "Spring".into(),
            segment_id: Some(42),
            status: "draft".into(),
            message_template: Some("Hello".into()),
            created_at: "2026-10-01T10:00:00Z".into(),
            started_at: None,
            completed_at: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"status\":\"draft\""));
    }

    #[test]
    fn campaign_stats_serializes() {
        let s = CampaignStats {
            total_recipients: 10,
            sent: 8,
            failed: 1,
            pending: 1,
            replied: 3,
            reply_rate: 0.375,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"reply_rate\":0.375"));
    }

    // ---- OR-02 / B3: campaign send executor unit tests ---------------------

    use crate::helpscout::{
        ConversationCreated, CreateConversationInput, FakeHelpScoutProvider, HelpScoutProvider,
    };

    /// Fresh DB with the M031 outreach tables applied.
    fn fresh_db_m031() -> (Connection, std::sync::Arc<std::sync::Mutex<Connection>>) {
        let conn = fresh_db();
        apply_m031(&conn).expect("apply M031");
        let arc = std::sync::Arc::new(std::sync::Mutex::new(conn));
        // Re-open the inner connection for synchronous setup; the Arc is
        // what send_batch uses.
        let inner = arc.lock().unwrap();
        // The mutex-locked connection is the same handle the tests query
        // through; just return a clone of the Arc + a fresh Connection
        // obtained by re-opening the file.
        drop(inner);
        // To avoid file-locking headaches, we re-use the Arc's connection
        // for synchronous setup via .lock().unwrap() at each test step.
        let placeholder = Connection::open_in_memory().unwrap();
        (placeholder, arc)
    }

    /// A campaign + recipients fixture used by the send_batch tests.
    fn seed_campaign(
        conn: &Connection,
        mailbox_id: i64,
        recipients: &[(i64, Option<i64>, Option<&str>)],
    ) -> i64 {
        // The recipients tuple is (local_id, remote_id, email).
        conn.execute(
            "INSERT INTO outreach_campaigns
                 (name, subject, body, mailbox_local_id, tags, status, segment_id,
                  segment_snapshot, created_at, queued_at, completed_at, updated_at)
             VALUES ('Test','Hi {{first_name}}','Body {{last_ticket_subject}}', ?1, '[]',
                     'queued', NULL, NULL, datetime('now'), datetime('now'), NULL,
                     datetime('now'))",
            params![mailbox_id],
        )
        .unwrap();
        let campaign_id = conn.last_insert_rowid();
        for (local, remote, email) in recipients {
            conn.execute(
                "INSERT INTO outreach_recipients
                     (campaign_id, customer_local_id, customer_remote_id, email, snapshot, state,
                      attempts, last_error, hs_conversation_remote_id, hs_conversation_number,
                      sent_at, replied_at)
                 VALUES (?1, ?2, ?3, ?4, '{}', 'queued', 0, NULL, NULL, NULL, NULL, NULL)",
                params![campaign_id, local, remote, email],
            )
            .unwrap();
        }
        campaign_id
    }

    #[test]
    fn skip_ineligible_parks_no_email_and_dnc() {
        let (conn, arc) = fresh_db_m031();
        let _ = conn;
        let c = arc.lock().unwrap();
        // mailbox + customers.
        c.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO customers (id, remote_id, first_name, last_name) VALUES
                (10, 1001, 'Ada', 'Lovelace'),
                (11, 1002, 'Grace', 'Hopper'),
                (12, 1003, 'Linus', 'Torvalds')",
            [],
        )
        .unwrap();
        // Recipient with no remote id (skip-able via no-email path? no — has
        // email but no remote id triggers the per-row permanent failure later).
        // Here we test the no-email + DNC skip paths.
        // Recipients: (10,1001,has-email) ; (11,1002,no-email) ; (12,1003,dnc)
        let cid = seed_campaign(&c, 1, &[(10, Some(1001), Some("a@b.co"))]);
        // Add the no-email one directly (the helper takes Option<&str>; pass
        // None for email).
        c.execute(
            "INSERT INTO outreach_recipients
                 (campaign_id, customer_local_id, customer_remote_id, email, snapshot, state)
             VALUES (?1, 11, 1002, NULL, '{}', 'queued')",
            params![cid],
        )
        .unwrap();
        // DNC the third.
        c.execute(
            "INSERT INTO do_not_contact (customer_id, reason) VALUES (10, 'manual')",
            [],
        )
        .unwrap();

        let skipped = skip_ineligible_recipients(&c, cid);
        // 2 skipped: customer 10 (DNC), customer 11 (no email).
        assert_eq!(skipped, 2);

        // Verify the rows are now state='skipped'.
        let parked: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM outreach_recipients
                  WHERE campaign_id = ?1 AND state = 'skipped'",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parked, 2);
    }

    #[test]
    fn pick_batch_returns_at_most_five_rows() {
        let (conn, arc) = fresh_db_m031();
        let _ = conn;
        let c = arc.lock().unwrap();
        c.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO customers (id, remote_id, first_name, last_name) VALUES
                (10, 1001, 'Ada', 'Lovelace')",
            [],
        )
        .unwrap();
        let cid = seed_campaign(&c, 1, &[(10, Some(1001), Some("a@b.co"))]);
        // Add 7 more recipients — total 8, pick_batch should return 5.
        for i in 11..18 {
            c.execute(
                "INSERT INTO outreach_recipients
                     (campaign_id, customer_local_id, customer_remote_id, email, snapshot, state)
                 VALUES (?1, ?2, ?3, ?4, '{}', 'queued')",
                params![cid, i, 1000 + i as i64, format!("u{i}@b.co")],
            )
            .unwrap();
        }
        let batch = pick_batch(&c, cid);
        assert_eq!(batch.len(), 5, "pick_batch returns at most 5");
        // Ordered by id ASC.
        assert!(batch[0].recipient_id < batch[1].recipient_id);
    }

    #[test]
    fn classify_error_permanent_for_4xx_caller_fault() {
        use crate::error::Error;
        use crate::helpscout_real::HsApiError;
        let e: Error = HsApiError {
            status_code: 400,
            message: "bad request".into(),
            friendly: "Invalid request".into(),
            retryable: false,
        }
        .into();
        match classify_error(&e, 0) {
            SendOutcome::FailedPermanent(_) => {}
            other => panic!("expected FailedPermanent, got {:?}", other.label()),
        }
    }

    #[test]
    fn classify_error_retryable_for_5xx_then_permanent_on_max_attempts() {
        use crate::error::Error;
        use crate::helpscout_real::HsApiError;
        let e: Error = HsApiError {
            status_code: 503,
            message: "unavailable".into(),
            friendly: "Help Scout is down".into(),
            retryable: true,
        }
        .into();
        assert!(
            matches!(classify_error(&e, 0), SendOutcome::FailedRetryable(_)),
            "5xx with attempts < max should be retryable"
        );
        match classify_error(&e, SEND_MAX_ATTEMPTS - 1) {
            SendOutcome::FailedPermanent(_) => {}
            other => panic!(
                "5xx with attempts = max-1 (next attempt = max) should be permanent, got {:?}",
                other.label()
            ),
        }
    }

    #[test]
    fn classify_error_unknown_for_2xx_no_id() {
        use crate::error::Error;
        use crate::helpscout_real::HsApiError;
        let e: Error = HsApiError {
            status_code: 200,
            message: "no id".into(),
            friendly: "Help Scout accepted the send but returned no id".into(),
            retryable: false,
        }
        .into();
        assert!(matches!(classify_error(&e, 0), SendOutcome::Unknown(_)));
    }

    #[tokio::test]
    async fn send_batch_happy_path_marks_sent_and_finalizes() {
        let (conn, arc) = fresh_db_m031();
        let _ = conn;
        {
            let c = arc.lock().unwrap();
            c.execute(
                "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO customers (id, remote_id, first_name, last_name) VALUES
                    (10, 1001, 'Ada', 'Lovelace'),
                    (11, 1002, 'Grace', 'Hopper'),
                    (12, 1003, 'Linus', 'Torvalds')",
                [],
            )
            .unwrap();
        }
        let cid = {
            let c = arc.lock().unwrap();
            seed_campaign(
                &c,
                1,
                &[
                    (10, Some(1001), Some("a@b.co")),
                    (11, Some(1002), Some("g@b.co")),
                    (12, Some(1003), Some("l@b.co")),
                ],
            )
        };

        let provider: std::sync::Arc<dyn HelpScoutProvider> =
            std::sync::Arc::new(FakeHelpScoutProvider::new_demo());
        let summary = send_batch(&arc, &provider, cid).await;

        // The summary claims the campaign is finalized.
        assert_eq!(summary["ok"].as_bool(), Some(true));
        assert_eq!(summary["remaining"].as_i64(), Some(0));

        let c = arc.lock().unwrap();
        // All three recipients are now 'sent' with a real hs_conversation_remote_id.
        let sent: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM outreach_recipients
                  WHERE campaign_id = ?1 AND state = 'sent'",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sent, 3, "all three recipients should be sent");

        // The new conversations exist in the local mirror (sync-back was
        // enqueued — the fake provider's create_conversation added them to
        // its in-memory world; we verify the outreach_recipients got
        // populated with the new remote ids + numbers).
        let with_remote: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM outreach_recipients
                  WHERE campaign_id = ?1
                    AND hs_conversation_remote_id IS NOT NULL
                    AND hs_conversation_number IS NOT NULL
                    AND sent_at IS NOT NULL",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(with_remote, 3);

        // Three outreach_attempts rows (one per send).
        let attempts: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM outreach_attempts
                  WHERE recipient_id IN (SELECT id FROM outreach_recipients WHERE campaign_id = ?1)
                    AND result = 'sent'",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(attempts, 3);

        // The campaign is 'completed'.
        let status: String = c
            .query_row(
                "SELECT status FROM outreach_campaigns WHERE id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "completed");

        // A sync_conversation job was enqueued per recipient (sync-back).
        let sync_jobs: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE type = 'sync_conversation' AND status = 'queued'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            sync_jobs, 3,
            "sync-back enqueues one job per sent recipient"
        );
    }

    #[tokio::test]
    async fn send_batch_marks_unknown_when_provider_returns_no_id() {
        // A stub provider whose create_conversation returns an HsApiError
        // with status 200 — the "sent but no id" sentinel. The recipient
        // should land in 'unknown' state.
        use crate::helpscout_real::{friendly_error, HsApiError};
        struct UnknownStub;
        #[async_trait::async_trait]
        impl HelpScoutProvider for UnknownStub {
            fn kind(&self) -> &'static str {
                "stub"
            }
            async fn get_me(&self) -> Result<crate::helpscout::HsUser> {
                Ok(crate::helpscout::HsUser::default())
            }
            async fn list_mailboxes(&self) -> Result<Vec<crate::helpscout::HsMailbox>> {
                Ok(Vec::new())
            }
            async fn list_users(&self) -> Result<Vec<crate::helpscout::HsUser>> {
                Ok(Vec::new())
            }
            async fn list_teams(&self) -> Result<Vec<crate::helpscout::HsTeam>> {
                Ok(Vec::new())
            }
            async fn list_tags(&self) -> Result<Vec<crate::helpscout::HsTag>> {
                Ok(Vec::new())
            }
            async fn list_conversations(
                &self,
                _q: &crate::helpscout::ConversationQuery,
            ) -> Result<crate::helpscout::Page<crate::helpscout::HsConversation>> {
                Ok(crate::helpscout::Page {
                    items: Vec::new(),
                    next_cursor: None,
                })
            }
            async fn list_customers(
                &self,
                _q: &crate::helpscout::CustomerQuery,
            ) -> Result<crate::helpscout::Page<crate::helpscout::HsCustomer>> {
                Ok(crate::helpscout::Page {
                    items: Vec::new(),
                    next_cursor: None,
                })
            }
            async fn list_beacon_chats(&self) -> Result<Vec<crate::helpscout::HsBeaconChat>> {
                Ok(Vec::new())
            }
            async fn list_docs(&self) -> Result<Vec<crate::helpscout::HsDocArticle>> {
                Ok(Vec::new())
            }
            async fn list_ratings(&self) -> Result<Vec<crate::helpscout::HsRating>> {
                Ok(Vec::new())
            }
            async fn create_reply_thread(
                &self,
                _i: crate::helpscout::CreateThreadInput,
            ) -> Result<crate::helpscout::ThreadCreated> {
                Ok(crate::helpscout::ThreadCreated {
                    thread_id: 0,
                    conversation_id: 0,
                })
            }
            async fn create_note_thread(
                &self,
                _i: crate::helpscout::CreateThreadInput,
            ) -> Result<crate::helpscout::ThreadCreated> {
                Ok(crate::helpscout::ThreadCreated {
                    thread_id: 0,
                    conversation_id: 0,
                })
            }
            async fn update_conversation(
                &self,
                _id: i64,
                _p: crate::helpscout::ConversationPatch,
            ) -> Result<bool> {
                Ok(true)
            }
            async fn create_conversation(
                &self,
                _i: CreateConversationInput,
            ) -> Result<ConversationCreated> {
                Err(crate::error::Error::Other(Box::new(HsApiError {
                    status_code: 200,
                    message: "no id".into(),
                    friendly: friendly_error(200, "", "POST"),
                    retryable: false,
                })))
            }
        }

        let (conn, arc) = fresh_db_m031();
        let _ = conn;
        {
            let c = arc.lock().unwrap();
            c.execute(
                "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO customers (id, remote_id, first_name, last_name) VALUES
                    (10, 1001, 'Ada', 'Lovelace')",
                [],
            )
            .unwrap();
        }
        let cid = {
            let c = arc.lock().unwrap();
            seed_campaign(&c, 1, &[(10, Some(1001), Some("a@b.co"))])
        };
        let provider: std::sync::Arc<dyn HelpScoutProvider> = std::sync::Arc::new(UnknownStub);
        let _summary = send_batch(&arc, &provider, cid).await;
        let c = arc.lock().unwrap();
        let state: String = c
            .query_row(
                "SELECT state FROM outreach_recipients WHERE campaign_id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "unknown", "200-no-id outcome should park as unknown");
        // campaign is still completed (no remaining recipients).
        let status: String = c
            .query_row(
                "SELECT status FROM outreach_campaigns WHERE id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    async fn send_batch_marks_failed_for_permanent_provider_error() {
        use crate::helpscout_real::HsApiError;
        struct FailStub;
        #[async_trait::async_trait]
        impl HelpScoutProvider for FailStub {
            fn kind(&self) -> &'static str {
                "stub"
            }
            async fn get_me(&self) -> Result<crate::helpscout::HsUser> {
                Ok(crate::helpscout::HsUser::default())
            }
            async fn list_mailboxes(&self) -> Result<Vec<crate::helpscout::HsMailbox>> {
                Ok(Vec::new())
            }
            async fn list_users(&self) -> Result<Vec<crate::helpscout::HsUser>> {
                Ok(Vec::new())
            }
            async fn list_teams(&self) -> Result<Vec<crate::helpscout::HsTeam>> {
                Ok(Vec::new())
            }
            async fn list_tags(&self) -> Result<Vec<crate::helpscout::HsTag>> {
                Ok(Vec::new())
            }
            async fn list_conversations(
                &self,
                _q: &crate::helpscout::ConversationQuery,
            ) -> Result<crate::helpscout::Page<crate::helpscout::HsConversation>> {
                Ok(crate::helpscout::Page {
                    items: Vec::new(),
                    next_cursor: None,
                })
            }
            async fn list_customers(
                &self,
                _q: &crate::helpscout::CustomerQuery,
            ) -> Result<crate::helpscout::Page<crate::helpscout::HsCustomer>> {
                Ok(crate::helpscout::Page {
                    items: Vec::new(),
                    next_cursor: None,
                })
            }
            async fn list_beacon_chats(&self) -> Result<Vec<crate::helpscout::HsBeaconChat>> {
                Ok(Vec::new())
            }
            async fn list_docs(&self) -> Result<Vec<crate::helpscout::HsDocArticle>> {
                Ok(Vec::new())
            }
            async fn list_ratings(&self) -> Result<Vec<crate::helpscout::HsRating>> {
                Ok(Vec::new())
            }
            async fn create_reply_thread(
                &self,
                _i: crate::helpscout::CreateThreadInput,
            ) -> Result<crate::helpscout::ThreadCreated> {
                Ok(crate::helpscout::ThreadCreated {
                    thread_id: 0,
                    conversation_id: 0,
                })
            }
            async fn create_note_thread(
                &self,
                _i: crate::helpscout::CreateThreadInput,
            ) -> Result<crate::helpscout::ThreadCreated> {
                Ok(crate::helpscout::ThreadCreated {
                    thread_id: 0,
                    conversation_id: 0,
                })
            }
            async fn update_conversation(
                &self,
                _id: i64,
                _p: crate::helpscout::ConversationPatch,
            ) -> Result<bool> {
                Ok(true)
            }
            async fn create_conversation(
                &self,
                _i: CreateConversationInput,
            ) -> Result<ConversationCreated> {
                Err(crate::error::Error::Other(Box::new(HsApiError {
                    status_code: 400,
                    message: "bad request".into(),
                    friendly: "Invalid request".into(),
                    retryable: false,
                })))
            }
        }

        let (conn, arc) = fresh_db_m031();
        let _ = conn;
        {
            let c = arc.lock().unwrap();
            c.execute(
                "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO customers (id, remote_id, first_name, last_name) VALUES
                    (10, 1001, 'Ada', 'Lovelace')",
                [],
            )
            .unwrap();
        }
        let cid = {
            let c = arc.lock().unwrap();
            seed_campaign(&c, 1, &[(10, Some(1001), Some("a@b.co"))])
        };
        let provider: std::sync::Arc<dyn HelpScoutProvider> = std::sync::Arc::new(FailStub);
        let _summary = send_batch(&arc, &provider, cid).await;
        let c = arc.lock().unwrap();
        let (state, last_error): (String, Option<String>) = c
            .query_row(
                "SELECT state, last_error FROM outreach_recipients WHERE campaign_id = ?1",
                params![cid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "failed", "400 → permanent failure");
        assert!(last_error.unwrap_or_default().contains("400"));
    }
}
