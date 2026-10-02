//! Help Scout mirror readouts — the reference's `referenceRepo` read surface
//! backed by mirror tables (`001_core`/`007_channels_docs` DDL, idempotent
//! creation). These tables are populated by the sync engine; on a fresh
//! install they read as empty, exactly like the reference before its first
//! sync.

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::error::Result;

/// Create every mirror table this module reads (idempotent).
pub fn ensure_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS inbox_fields (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            type TEXT,
            system_type TEXT,
            required INTEGER DEFAULT 0,
            sort_order INTEGER DEFAULT 0,
            raw_json TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_inbox_fields_mailbox ON inbox_fields(mailbox_id);
        CREATE TABLE IF NOT EXISTS inbox_field_options (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            field_id INTEGER NOT NULL REFERENCES inbox_fields(id) ON DELETE CASCADE,
            label TEXT,
            sort_order INTEGER DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS saved_replies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            mailbox_local_id INTEGER REFERENCES mailboxes(id),
            name TEXT NOT NULL,
            preview TEXT,
            text TEXT,
            chat_text TEXT,
            raw_json TEXT,
            remote_updated_at TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_saved_replies_name ON saved_replies(name);
        CREATE TABLE IF NOT EXISTS workflows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            mailbox_local_id INTEGER REFERENCES mailboxes(id),
            name TEXT NOT NULL,
            type TEXT,
            status TEXT,
            sort_order INTEGER,
            raw_json TEXT,
            remote_updated_at TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );
        CREATE TABLE IF NOT EXISTS user_statuses (
            user_local_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
            email_status TEXT,
            email_updated_at TEXT,
            chat_status TEXT,
            mailbox_statuses TEXT,
            raw_json TEXT,
            last_synced_at TEXT
        );
        CREATE TABLE IF NOT EXISTS webhook_configs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            url TEXT,
            events TEXT,
            status TEXT,
            raw_json TEXT,
            last_synced_at TEXT
        );",
    )?;
    Ok(())
}

/// `getInboxFields()` — fields with their options.
pub fn inbox_fields(conn: &Connection) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let mut stmt = conn.prepare(
        "SELECT f.id FROM inbox_fields f WHERE f.deleted_at IS NULL ORDER BY f.mailbox_id, f.sort_order",
    )?;
    let ids: Vec<i64> = stmt
        .query_map([], |r| r.get::<_, i64>(0))?
        .flatten()
        .collect();
    drop(stmt);
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let field = conn.query_row(
            "SELECT f.id, f.remote_id, f.mailbox_id, f.name, f.type, f.system_type, f.required, f.sort_order AS \"order\"
             FROM inbox_fields f WHERE f.id = ?1",
            [id],
            field_row,
        )?;
        let mut opt_stmt = conn.prepare(
            "SELECT id, remote_id, label, sort_order FROM inbox_field_options WHERE field_id = ?1 ORDER BY sort_order",
        )?;
        let options: Vec<Value> = opt_stmt
            .query_map([id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "remote_id": r.get::<_, Option<i64>>(1)?,
                    "label": r.get::<_, Option<String>>(2)?,
                    "sort_order": r.get::<_, i64>(3)?,
                }))
            })?
            .flatten()
            .collect();
        let mut with_options = field;
        with_options["options"] = Value::Array(options);
        out.push(with_options);
    }
    Ok(out)
}

/// One inbox-field row as JSON.
fn field_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "remote_id": r.get::<_, Option<i64>>(1)?,
        "mailbox_id": r.get::<_, i64>(2)?,
        "name": r.get::<_, String>(3)?,
        "type": r.get::<_, Option<String>>(4)?,
        "system_type": r.get::<_, Option<String>>(5)?,
        "required": r.get::<_, i64>(6)?,
        "order": r.get::<_, i64>(7)?,
    }))
}

/// `getWorkflows()`.
pub fn workflows(conn: &Connection) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, mailbox_local_id AS mailbox_id, name, type, status, sort_order AS \"order\"
         FROM workflows WHERE deleted_at IS NULL ORDER BY sort_order",
    )?;
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "remote_id": r.get::<_, Option<i64>>(1)?,
                "mailbox_id": r.get::<_, Option<i64>>(2)?,
                "name": r.get::<_, String>(3)?,
                "type": r.get::<_, Option<String>>(4)?,
                "status": r.get::<_, Option<String>>(5)?,
                "order": r.get::<_, Option<i64>>(6)?,
            }))
        })?
        .flatten()
        .collect();
    Ok(rows)
}

/// `getUserStatuses()` — mailbox_statuses parsed from JSON text.
pub fn user_statuses(conn: &Connection) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let mut stmt = conn.prepare(
        "SELECT user_local_id AS user_id, email_status, chat_status, mailbox_statuses FROM user_statuses
         JOIN users ON users.id = user_statuses.user_local_id WHERE users.deleted_at IS NULL",
    )?;
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            let mailbox_statuses: Option<String> = r.get(3)?;
            let parsed: Value = mailbox_statuses
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| json!({}));
            Ok(json!({
                "user_id": r.get::<_, i64>(0)?,
                "email_status": r.get::<_, Option<String>>(1)?,
                "chat_status": r.get::<_, Option<String>>(2)?,
                "mailbox_statuses": parsed,
            }))
        })?
        .flatten()
        .collect();
    Ok(rows)
}

/// `getWebhookConfigs()` — events parsed from JSON array text.
pub fn webhook_configs(conn: &Connection) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let mut stmt =
        conn.prepare("SELECT id, remote_id, url, events, status FROM webhook_configs")?;
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            let events: Option<String> = r.get(3)?;
            let parsed: Value = events
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| json!([]));
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "remote_id": r.get::<_, Option<i64>>(1)?,
                "url": r.get::<_, Option<String>>(2)?,
                "events": parsed,
                "status": r.get::<_, Option<String>>(4)?,
            }))
        })?
        .flatten()
        .collect();
    Ok(rows)
}

/// `getSavedReplies()` / `searchSavedReplies(q)`.
pub fn saved_replies(conn: &Connection, q: Option<&str>) -> Result<Vec<Value>> {
    ensure_tables(conn)?;
    let base_sql = "SELECT id, remote_id, mailbox_local_id, name, preview, text, chat_text FROM saved_replies WHERE deleted_at IS NULL";
    if let Some(q) = q.filter(|s| !s.is_empty()) {
        // Reference: case-insensitive contains on name (referenceRepo.searchSavedReplies).
        let mut stmt = conn.prepare(&format!(
            "{base_sql} AND lower(name) LIKE '%' || lower(?1) || '%' ORDER BY name"
        ))?;
        let out: Vec<Value> = stmt.query_map([q], reply_row)?.flatten().collect();
        Ok(out)
    } else {
        let mut stmt = conn.prepare(&format!("{base_sql} ORDER BY name"))?;
        let out: Vec<Value> = stmt.query_map([], reply_row)?.flatten().collect();
        Ok(out)
    }
}

fn reply_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "remote_id": r.get::<_, Option<i64>>(1)?,
        "mailbox_id": r.get::<_, Option<i64>>(2)?,
        "name": r.get::<_, String>(3)?,
        "preview": r.get::<_, Option<String>>(4)?,
        "text": r.get::<_, Option<String>>(5)?,
        "chat_text": r.get::<_, Option<String>>(6)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mailboxes (id INTEGER PRIMARY KEY, deleted_at TEXT);
             CREATE TABLE users (id INTEGER PRIMARY KEY, deleted_at TEXT);",
        )
        .unwrap();
        ensure_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn reads_empty_on_fresh_db_and_roundtrips() {
        let conn = setup();
        assert!(inbox_fields(&conn).unwrap().is_empty());
        assert!(workflows(&conn).unwrap().is_empty());
        assert!(user_statuses(&conn).unwrap().is_empty());
        assert!(webhook_configs(&conn).unwrap().is_empty());
        assert!(saved_replies(&conn, None).unwrap().is_empty());

        conn.execute("INSERT INTO users (id) VALUES (1)", [])
            .unwrap();
        conn.execute("INSERT INTO mailboxes (id) VALUES (1)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO saved_replies (remote_id, mailbox_local_id, name, preview) VALUES (10, 1, 'Greetings', 'Hello there')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO user_statuses (user_local_id, email_status, mailbox_statuses) VALUES (1, 'active', '{\"1\":\"available\"}')",
            [],
        )
        .unwrap();
        let replies = saved_replies(&conn, None).unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["name"], "Greetings");
        let searched = saved_replies(&conn, Some("greet")).unwrap();
        assert_eq!(searched.len(), 1);
        assert!(saved_replies(&conn, Some("zzz")).unwrap().is_empty());
        let statuses = user_statuses(&conn).unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["mailbox_statuses"]["1"], "available");
    }
}
