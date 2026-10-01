//! Outreach — segmentation, campaigns, do-not-contact, monitoring (M9).
//!
//! Per spec M9: "Outreach: segmentation, saved segments, campaigns,
//! do-not-contact, monitoring."
//! Per KNOWN PITFALLS: "Campaign recipients that exhaust retries must fail,
//! never livelock; 'retry failed' resets the attempt budget."

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

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
}
