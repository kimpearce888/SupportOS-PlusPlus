//! Audit log + application errors — mirrors the reference's `jobRepo`
//! audit/error surface (`src/server/database/repositories/jobRepo.ts`) and
//! the `audit_log` / `application_errors` tables from reference migration
//! `002_sync_jobs.ts` (identical DDL, idempotent creation).

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::error::Result;

/// Ensure the `audit_log` and `application_errors` tables exist with the
/// reference's exact DDL (migration 002_sync_jobs.ts lines 104-127).
pub fn ensure_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            actor TEXT NOT NULL,
            action TEXT NOT NULL,
            conversation_id INTEGER,
            before_state TEXT,
            after_state TEXT,
            remote_operation TEXT,
            remote_result TEXT,
            ai_involvement INTEGER DEFAULT 0,
            job_id INTEGER,
            correlation_id TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_audit_conversation ON audit_log(conversation_id);
        CREATE INDEX IF NOT EXISTS idx_audit_time ON audit_log(timestamp DESC);

        CREATE TABLE IF NOT EXISTS application_errors (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            service TEXT,
            message TEXT,
            stack TEXT,
            context TEXT
        );",
    )?;
    Ok(())
}

/// The reference's audit entry vocabulary: `actor` is a closed set
/// (`jobRepo.ts:203`).
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub actor: &'static str,
    pub action: String,
    pub conversation_id: Option<i64>,
    pub before_state: Option<Value>,
    pub after_state: Option<Value>,
    pub remote_operation: Option<String>,
    pub remote_result: Option<Value>,
    pub ai_involvement: bool,
    pub job_id: Option<i64>,
    pub correlation_id: Option<String>,
}

impl AuditEntry {
    pub fn user(action: &str) -> Self {
        Self {
            actor: "user",
            action: action.to_string(),
            conversation_id: None,
            before_state: None,
            after_state: None,
            remote_operation: None,
            remote_result: None,
            ai_involvement: false,
            job_id: None,
            correlation_id: None,
        }
    }

    pub fn with_after_state(mut self, state: Value) -> Self {
        self.after_state = Some(state);
        self
    }

    /// Attach a before-state snapshot (delete-style audit entries).
    pub fn with_before_state(mut self, state: Value) -> Self {
        self.before_state = Some(state);
        self
    }
}

/// Record an audit entry — mirrors `jobRepo.audit()` exactly: states are
/// JSON-serialized and truncated to 4000 chars; `ai_involvement` is stored
/// as 0/1.
pub fn audit(conn: &Connection, entry: &AuditEntry) -> Result<()> {
    ensure_tables(conn)?;
    let before = entry
        .before_state
        .as_ref()
        .map(|v| v.to_string())
        .map(|s| s.chars().take(4000).collect::<String>());
    let after = entry
        .after_state
        .as_ref()
        .map(|v| v.to_string())
        .map(|s| s.chars().take(4000).collect::<String>());
    let remote_result = entry.remote_result.as_ref().map(|v| v.to_string());
    conn.execute(
        "INSERT INTO audit_log (timestamp, actor, action, conversation_id, before_state, after_state, remote_operation, remote_result, ai_involvement, job_id, correlation_id)
         VALUES (datetime('now'), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            entry.actor,
            entry.action,
            entry.conversation_id,
            before,
            after,
            entry.remote_operation,
            remote_result,
            i64::from(entry.ai_involvement),
            entry.job_id,
            entry.correlation_id,
        ],
    )?;
    Ok(())
}

/// `jobRepo.listAudit(conversationId?, limit=200)` — newest first.
pub fn list_audit(
    conn: &Connection,
    conversation_id: Option<i64>,
    limit: i64,
) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let mut out = Vec::new();
    if let Some(cid) = conversation_id {
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, actor, action, conversation_id, before_state, after_state, remote_operation, remote_result, ai_involvement, job_id, correlation_id
             FROM audit_log WHERE conversation_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![cid, limit], audit_row_to_json)?;
        out.extend(rows.flatten());
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, actor, action, conversation_id, before_state, after_state, remote_operation, remote_result, ai_involvement, job_id, correlation_id
             FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], audit_row_to_json)?;
        out.extend(rows.flatten());
    }
    Ok(out)
}

fn audit_row_to_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let before: Option<String> = r.get(5)?;
    let after: Option<String> = r.get(6)?;
    let remote_result: Option<String> = r.get(8)?;
    let parse_json = |s: Option<&str>| -> Option<Value> {
        s.and_then(|v| serde_json::from_str::<Value>(v).ok())
    };
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "timestamp": r.get::<_, String>(1)?,
        "actor": r.get::<_, String>(2)?,
        "action": r.get::<_, String>(3)?,
        "conversation_id": r.get::<_, Option<i64>>(4)?,
        "before_state": parse_json(before.as_deref()),
        "after_state": parse_json(after.as_deref()),
        "remote_operation": r.get::<_, Option<String>>(7)?,
        "remote_result": parse_json(remote_result.as_deref()),
        "ai_involvement": r.get::<_, i64>(9)? != 0,
        "job_id": r.get::<_, Option<i64>>(10)?,
        "correlation_id": r.get::<_, Option<String>>(11)?,
    }))
}

/// `jobRepo.logError(service, message, stack?, context?)` — message truncated
/// to 2000 chars, stack to 8000.
pub fn log_error(
    conn: &Connection,
    service: &str,
    message: &str,
    stack: Option<&str>,
    context: Option<&Value>,
) -> Result<()> {
    ensure_tables(conn)?;
    let msg: String = message.chars().take(2000).collect();
    let stk: Option<String> = stack.map(|s| s.chars().take(8000).collect());
    conn.execute(
        "INSERT INTO application_errors (timestamp, service, message, stack, context)
         VALUES (datetime('now'), ?1, ?2, ?3, ?4)",
        rusqlite::params![service, msg, stk, context.map(|c| c.to_string()),],
    )?;
    Ok(())
}

/// `jobRepo.listRecentErrors(limit=50)` — id/timestamp/service/message only.
pub fn list_recent_errors(conn: &Connection, limit: i64) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, service, message FROM application_errors ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit], |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "timestamp": r.get::<_, String>(1)?,
            "service": r.get::<_, Option<String>>(2)?,
            "message": r.get::<_, Option<String>>(3)?,
        }))
    })?;
    Ok(rows.flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_roundtrip_and_listing() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();
        audit(
            &conn,
            &AuditEntry::user("business_hours_updated").with_after_state(json!({"mailboxId": 1})),
        )
        .unwrap();
        audit(
            &conn,
            &AuditEntry {
                actor: "ai",
                action: "test".into(),
                conversation_id: Some(42),
                before_state: None,
                after_state: Some(json!({"x": 1})),
                remote_operation: None,
                remote_result: None,
                ai_involvement: true,
                job_id: None,
                correlation_id: None,
            },
        )
        .unwrap();
        let all = list_audit(&conn, None, 200).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["action"], "test"); // newest first
        assert_eq!(all[0]["ai_involvement"], true);
        let filtered = list_audit(&conn, Some(42), 200).unwrap();
        assert_eq!(filtered.len(), 1);
        log_error(&conn, "http", "boom", None, Some(&json!({"path": "/x"}))).unwrap();
        let errs = list_recent_errors(&conn, 50).unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["service"], "http");
    }
}
