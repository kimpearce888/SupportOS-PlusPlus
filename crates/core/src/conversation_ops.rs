//! Conversation operations service — port of the reference
//! `src/server/services/operations.ts` write pipeline.
//!
//! Reference pipeline order (spec): validate → authorize → fresh-read →
//! merge → write → confirm → persist → audit. Demo mode writes the local
//! database directly (the port's fake provider IS the local store — the
//! reference's fakeProvider writes its world, then the service persists
//! locally; both effects collapse into one local write here, which is
//! observably equivalent). Real mode calls Help Scout first, then persists.
//!
//! Every message below is byte-identical to the reference service so the
//! differential harness can compare responses field-by-field.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::error::Result;

// ---------------------------------------------------------------------------
// M030 schema — columns/tables the reference ops require
// ---------------------------------------------------------------------------

/// Apply the M030 batch: `snoozed_until` on conversations, the
/// `conversation_tags` + `conversation_fields` join tables, thread
/// scheduling columns and the `attachments` table (reference 001_core.ts:
/// 288-401 shapes, adapted to the port's existing column names).
pub fn apply_m030(conn: &Connection) -> Result<()> {
    ensure_column(
        conn,
        "conversations",
        "snoozed_until",
        "ALTER TABLE conversations ADD COLUMN snoozed_until TEXT",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversation_tags (
            conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            tag_id         INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (conversation_id, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_tags_tag
            ON conversation_tags(tag_id);
        CREATE TABLE IF NOT EXISTS conversation_fields (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            field_id        INTEGER NOT NULL,
            value           TEXT,
            text_value      TEXT,
            UNIQUE (conversation_id, field_id)
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_fields_conv
            ON conversation_fields(conversation_id);
        CREATE TABLE IF NOT EXISTS attachments (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id       INTEGER UNIQUE,
            thread_id       INTEGER,
            conversation_id INTEGER NOT NULL,
            filename        TEXT,
            mime_type       TEXT,
            size            INTEGER,
            local_path      TEXT,
            hash            TEXT,
            downloaded_at   TEXT,
            state           TEXT DEFAULT 'metadata',
            raw_json        TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_attachments_conversation
            ON attachments(conversation_id);",
    )?;
    ensure_column(
        conn,
        "conversation_threads",
        "state",
        "ALTER TABLE conversation_threads ADD COLUMN state TEXT DEFAULT 'published'",
    )?;
    ensure_column(
        conn,
        "conversation_threads",
        "scheduled_for",
        "ALTER TABLE conversation_threads ADD COLUMN scheduled_for TEXT",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 30 WHERE id = 1", []);
    Ok(())
}

/// Add a column only when missing (idempotent boot-time batch helper).
fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<()> {
    let present = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|c| c.ok())
        .any(|c| c == column);
    if !present {
        conn.execute_batch(ddl)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Zod-parity validation helpers (reference app.ts:164-186 error envelope)
// ---------------------------------------------------------------------------

/// JavaScript `Number(value)` semantics: trimmed string parse, '' → 0,
/// non-numeric → NaN (represented as `None`).
pub fn js_number(raw: &str) -> Option<f64> {
    let t = raw.trim();
    if t.is_empty() {
        return Some(0.0);
    }
    t.parse::<f64>().ok()
}

/// The reference 422 envelope for Zod failures (first issue only, as the
/// route-level parse surfaces one object's first issue).
pub fn zod_422(path: &str, message: &str) -> Response {
    zod_422_multi(&[(path, message)])
}

/// Multi-issue Zod envelope: the reference error handler lists up to 10
/// issues (path + message each) and surfaces the first in `message`.
pub fn zod_422_multi(issues: &[(&str, &str)]) -> Response {
    let first = issues
        .first()
        .map(|(p, m)| {
            let location = if p.is_empty() {
                String::new()
            } else {
                format!(" ({p})")
            };
            format!("Invalid request{location}: {m}")
        })
        .unwrap_or_else(|| "Invalid request: request body failed validation.".into());
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": first,
            "issues": issues.iter().take(10).map(|(p, m)| json!({
                "path": p, "message": m
            })).collect::<Vec<_>>()
        })),
    )
        .into_response()
}

/// Zod enum issue text: `Expected 'a' | 'b', received 'x'`.
pub fn zod_enum_message(variants: &[&str], received: &str) -> String {
    let expected = variants
        .iter()
        .map(|v| format!("'{v}'"))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("Invalid enum value. Expected {expected}, received '{received}'")
}

/// Validate a required integer field the way `z.number().int()` does,
/// producing the reference's exact issue text.
pub fn zod_int_msg(body: &Value, field: &str) -> std::result::Result<i64, String> {
    match body.get(field) {
        None | Some(Value::Null) => Err("Required".into()),
        Some(v) if v.is_number() => {
            let f = v.as_f64().unwrap();
            if f.fract() != 0.0 || f < i64::MIN as f64 || f > i64::MAX as f64 {
                Err("Expected integer, received float".into())
            } else {
                Ok(f as i64)
            }
        }
        Some(v) => Err(format!("Expected number, received {}", zod_typeof(v))),
    }
}

/// Validate a required string field the way `z.string()` does.
pub fn zod_string_msg(body: &Value, field: &str) -> std::result::Result<String, String> {
    match body.get(field) {
        None | Some(Value::Null) => Err("Required".into()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(v) => Err(format!("Expected string, received {}", zod_typeof(v))),
    }
}

/// Validate a required boolean field the way `z.boolean()` does.
pub fn zod_bool_msg(body: &Value, field: &str) -> std::result::Result<bool, String> {
    match body.get(field) {
        None | Some(Value::Null) => Err("Required".into()),
        Some(Value::Bool(b)) => Ok(*b),
        Some(v) => Err(format!("Expected boolean, received {}", zod_typeof(v))),
    }
}

fn zod_typeof(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
    }
}

// ---------------------------------------------------------------------------
// AI evaluation-mode guard (reference operations.ts:62-66)
// ---------------------------------------------------------------------------

/// The reference blocks every remote write when evaluation mode is ON.
pub fn eval_mode_blocked(conn: &Connection) -> Option<&'static str> {
    let on: Option<String> = crate::settings::get_string(conn, "ai_evaluation_mode")
        .ok()
        .flatten();
    matches!(on.as_deref(), Some("1") | Some("true") | Some("on"))
        .then_some(
            "AI evaluation mode is ON: no replies, notes, status changes or other updates are sent to Help Scout.",
        )
}

// ---------------------------------------------------------------------------
// Outbound job helpers (reference jobRepo.ts:167-196)
// ---------------------------------------------------------------------------

/// `createOutboundJob(kind, payload, opts)` — inserts a queued job and
/// returns its id. Errors are non-fatal (best-effort, like the reference's
/// in-process call that cannot realistically fail).
pub fn create_outbound_job(
    conn: &Connection,
    kind: &str,
    payload: &Value,
    conversation_id: Option<i64>,
    thread_id: Option<i64>,
) -> i64 {
    let payload = serde_json::to_string(payload).unwrap_or_else(|_| "{}".into());
    let res = conn.execute(
        "INSERT INTO outbound_jobs (kind, conversation_id, thread_id, payload,
             status, requires_confirmation, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'queued', 0, datetime('now'), datetime('now'))",
        params![kind, conversation_id, thread_id, payload],
    );
    if res.is_err() {
        return 0;
    }
    conn.last_insert_rowid()
}

/// `setOutboundStatus(id, status, error?)` — confirm/fail with attempt bump.
pub fn set_outbound_status(conn: &Connection, id: i64, status: &str, error: Option<&str>) {
    if id == 0 {
        return;
    }
    let _ = conn.execute(
        "UPDATE outbound_jobs
            SET status = ?1, error = ?2, attempts = attempts + 1,
                updated_at = datetime('now')
          WHERE id = ?3",
        params![status, error, id],
    );
}

// ---------------------------------------------------------------------------
// Shared lookups
// ---------------------------------------------------------------------------

/// Row shape the ops need for a conversation (local id + remote id).
pub struct ConvRef {
    pub id: i64,
    pub remote_id: i64,
    pub mailbox_id: i64,
}

/// `getConversationByLocalId` — the reference ops work on the LOCAL id and
/// require a remote mapping.
pub fn conv_by_local_id(conn: &Connection, id: i64) -> Option<ConvRef> {
    conn.query_row(
        "SELECT id, remote_id, mailbox_id FROM conversations WHERE id = ?1",
        params![id],
        |r| {
            Ok(ConvRef {
                id: r.get(0)?,
                remote_id: r.get(1)?,
                mailbox_id: r.get(2)?,
            })
        },
    )
    .ok()
}

/// Standard `{ok:false}` rejection with 422 (reference route wrapper:
/// `if (!result.ok) reply.code(422); return result;`).
pub fn rejected(message: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "ok": false, "message": message })),
    )
        .into_response()
}

/// Standard `{ok:...}` success response (200).
pub fn ok_result(message: &str, data: Option<Value>) -> Response {
    let mut body = json!({ "ok": true, "message": message });
    if let Some(d) = data {
        body["data"] = d;
    }
    (StatusCode::OK, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// move (operations.ts:245)
// ---------------------------------------------------------------------------

/// Move a conversation to another inbox. Local write + outbound job + audit.
pub fn op_move_to_inbox(
    conn: &Connection,
    conversation_local_id: i64,
    mailbox_id: i64,
) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation not found locally.");
    };
    let job_id = create_outbound_job(
        conn,
        "move",
        &json!({ "conversationId": conv.id, "mailboxRemoteId": mailbox_id }),
        Some(conv.id),
        None,
    );
    // Demo mode: the local DB is the remote. Real mode routes call the
    // provider before reaching here (see routes layer).
    let mailbox_name: Option<String> = conn
        .query_row(
            "SELECT name FROM mailboxes WHERE remote_id = ?1",
            params![mailbox_id],
            |r| r.get(0),
        )
        .ok();
    let before_mailbox = conv.mailbox_id;
    let updated = conn.execute(
        "UPDATE conversations SET mailbox_id =
             (SELECT id FROM mailboxes WHERE remote_id = ?1),
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id = ?2",
        params![mailbox_id, conv.id],
    );
    match updated {
        Ok(_) if mailbox_name.is_some() || updated.unwrap_or(0) > 0 => {
            set_outbound_status(conn, job_id, "confirmed", None);
            let _ = crate::jobs::audit(
                conn,
                "user",
                "moved_inbox",
                Some(conv.id),
                Some(&json!({ "mailbox": before_mailbox }).to_string()),
                Some(&json!({ "mailbox": mailbox_id }).to_string()),
                Some(&format!(
                    "PATCH /v2/conversations/{}/mailboxId",
                    conv.remote_id
                )),
                Some(&json!({ "job_id": job_id }).to_string()),
                false,
            );
            let name = mailbox_name.unwrap_or_else(|| "the new inbox".into());
            ok_result(&format!("Moved to {name}."), None)
        }
        _ => {
            let msg = "Conversation not found".to_string();
            set_outbound_status(conn, job_id, "failed", Some(&msg));
            rejected(&format!("Conversation was NOT moved. {msg}"))
        }
    }
}

// ---------------------------------------------------------------------------
// tags (operations.ts:269 — fresh-read-merge-write, spec #18)
// ---------------------------------------------------------------------------

/// Merge tag change against current state. `set` replaces; otherwise
/// current-minus-remove plus add, deduplicated, trimmed, non-empty.
pub fn op_update_tags(conn: &Connection, conversation_local_id: i64, change: &Value) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation not found locally.");
    };
    let job_id = create_outbound_job(
        conn,
        "update_tags",
        &json!({ "conversationId": conv.id, "change": change }),
        Some(conv.id),
        None,
    );
    // 1. read LATEST state (demo: local tags ARE the remote truth)
    let current: Vec<String> = read_conversation_tags(conn, conv.id);
    // 2. desired state
    let desired: Vec<String> = if let Some(set) = change.get("set").and_then(|v| v.as_array()) {
        let mut seen = std::collections::HashSet::new();
        set.iter()
            .filter_map(|t| t.as_str())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && seen.insert(t.clone()))
            .collect()
    } else {
        let remove: Vec<String> = change
            .get("remove")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str())
                    .map(|t| t.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let add: Vec<String> = change
            .get("add")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str())
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        current
            .iter()
            .filter(|t| !remove.contains(&t.to_lowercase()))
            .chain(add.iter())
            .filter(|t| seen.insert((*t).clone()))
            .cloned()
            .collect()
    };
    // 3. send complete state (demo: local write)
    write_conversation_tags(conn, conv.id, &desired);
    // 4. confirm + persist + audit
    set_outbound_status(conn, job_id, "confirmed", None);
    let _ = crate::jobs::audit(
        conn,
        "user",
        "tags_changed",
        Some(conv.id),
        Some(&json!({ "tags": current }).to_string()),
        Some(&json!({ "tags": desired }).to_string()),
        Some(&format!("PUT /v2/conversations/{}/tags", conv.remote_id)),
        Some(&json!({ "job_id": job_id }).to_string()),
        false,
    );
    ok_result("Tags updated.", Some(json!({ "tags": desired })))
}

/// Current tag names for a conversation (local ids resolved to names).
pub fn read_conversation_tags(conn: &Connection, conversation_id: i64) -> Vec<String> {
    let mut stmt = match conn.prepare(
        "SELECT t.name FROM conversation_tags ct
            JOIN tags t ON t.id = ct.tag_id
           WHERE ct.conversation_id = ?1
           ORDER BY t.name",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![conversation_id], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Pure merge used by the real-provider path (current remote truth + change).
pub fn merge_tags(current: &[String], change: &Value) -> Vec<String> {
    if let Some(set) = change.get("set").and_then(|v| v.as_array()) {
        let mut seen = std::collections::HashSet::new();
        return set
            .iter()
            .filter_map(|t| t.as_str())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && seen.insert(t.clone()))
            .collect();
    }
    let remove: Vec<String> = change
        .get("remove")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str())
                .map(|t| t.to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let add: Vec<String> = change
        .get("add")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    current
        .iter()
        .filter(|t| !remove.contains(&t.to_lowercase()))
        .chain(add.iter())
        .filter(|t| seen.insert((*t).clone()))
        .cloned()
        .collect()
}

/// Replace the tag set, mirroring the reference `updateLocalTags`
/// (conversationRepo.ts:899): case-insensitive name lookup, unknown names
/// inserted with monotonic NEGATIVE remote ids, slugified names.
pub fn write_conversation_tags(conn: &Connection, conversation_id: i64, tags: &[String]) {
    let _ = conn.execute(
        "DELETE FROM conversation_tags WHERE conversation_id = ?1",
        params![conversation_id],
    );
    for name in tags {
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE",
                params![name],
                |r| r.get(0),
            )
            .ok();
        let tag_id = existing.unwrap_or_else(|| {
            let next_local: i64 = conn
                .query_row("SELECT COALESCE(MAX(id), 0) + 1 AS n FROM tags", [], |r| {
                    r.get(0)
                })
                .unwrap_or(1);
            let _ = conn.execute(
                "INSERT INTO tags (remote_id, name, slug, ticket_count)
                 VALUES (?1, ?2, ?3, 0)",
                params![-next_local, name, slugify(name)],
            );
            conn.last_insert_rowid()
        });
        let _ = conn.execute(
            "INSERT OR IGNORE INTO conversation_tags (conversation_id, tag_id)
             VALUES (?1, ?2)",
            params![conversation_id, tag_id],
        );
    }
}

/// `name.toLowerCase().replace(/[^a-z0-9]+/g, '-')` (reference slug rule).
fn slugify(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut out = String::new();
    let mut dash = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// custom fields (operations.ts:307 — fresh-read-merge-write, spec #17)
// ---------------------------------------------------------------------------

/// Update custom fields. System fields (defined on inbox fields with
/// system_type) are preserved when omitted; user fields use replacement
/// semantics merged with the current user-field set.
pub fn op_update_custom_fields(
    conn: &Connection,
    conversation_local_id: i64,
    fields: &[(i64, Option<String>)],
) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation not found locally.");
    };
    let job_id = create_outbound_job(
        conn,
        "update_fields",
        &json!({ "conversationId": conv.id, "fields": fields }),
        Some(conv.id),
        None,
    );
    // Current state, split into system (preserved) + user (replaced).
    let current = read_conversation_fields(conn, conv.id);
    let changed_ids: std::collections::HashSet<i64> = fields.iter().map(|f| f.0).collect();
    let mut merged: Vec<Value> = Vec::new();
    for f in &current {
        let is_system = f
            .get("system_type")
            .and_then(|s| s.as_str())
            .map(|s| !s.is_empty() && s != "null")
            .unwrap_or(false);
        let changed = changed_ids.contains(&f["field_id"].as_i64().unwrap_or(-1));
        // System fields are always preserved; user fields survive only when
        // not replaced by this request.
        if is_system || !changed {
            merged.push(json!({ "id": f["field_id"], "value": f["value"] }));
        }
    }
    for (id, value) in fields {
        merged.push(json!({ "id": id, "value": value.clone().unwrap_or_default() }));
    }
    // send complete state (demo: local write)
    for (id, value) in fields {
        let text = resolve_field_text(conn, *id, value);
        let _ = conn.execute(
            "INSERT INTO conversation_fields (conversation_id, field_id, value, text_value)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(conversation_id, field_id)
             DO UPDATE SET value = excluded.value, text_value = excluded.text_value",
            params![conv.id, id, value.clone().unwrap_or_default(), text],
        );
    }
    set_outbound_status(conn, job_id, "confirmed", None);
    let _ = crate::jobs::audit(
        conn,
        "user",
        "fields_changed",
        Some(conv.id),
        Some(&json!({ "fields": current }).to_string()),
        Some(&json!({ "fields": merged }).to_string()),
        Some(&format!("PUT /v2/conversations/{}/fields", conv.remote_id)),
        Some(&json!({ "job_id": job_id }).to_string()),
        false,
    );
    ok_result("Custom fields updated.", None)
}

/// Current field values (with system_type classification for merge logic).
fn read_conversation_fields(conn: &Connection, conversation_id: i64) -> Vec<Value> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT cf.field_id, cf.value, cf.text_value,
                (SELECT system_type FROM inbox_fields WHERE remote_id = cf.field_id)
           FROM conversation_fields cf
          WHERE cf.conversation_id = ?1
          ORDER BY cf.field_id",
    ) else {
        return Vec::new();
    };
    let rows = stmt
        .query_map(params![conversation_id], |r| {
            Ok(json!({
                "field_id": r.get::<_, i64>(0)?,
                "value": r.get::<_, Option<String>>(1)?,
                "text_value": r.get::<_, Option<String>>(2)?,
                "system_type": r.get::<_, Option<String>>(3)?,
            }))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    rows
}

/// Resolve a field's display text (option label lookup).
fn resolve_field_text(conn: &Connection, field_id: i64, value: &Option<String>) -> Option<String> {
    let raw = value.clone()?;
    let label: Option<String> = conn
        .query_row(
            "SELECT o.label FROM inbox_field_options o
              JOIN inbox_fields f ON f.id = o.field_id
              WHERE f.remote_id = ?1 AND CAST(o.id AS TEXT) = ?2",
            params![field_id, raw],
            |r| r.get(0),
        )
        .ok();
    label.or(Some(raw))
}

// ---------------------------------------------------------------------------
// snooze / unsnooze (operations.ts:345, 366)
// ---------------------------------------------------------------------------

pub fn op_snooze(conn: &Connection, conversation_local_id: i64, snoozed_until: &str) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation not found locally.");
    };
    let job_id = create_outbound_job(
        conn,
        "snooze",
        &json!({ "conversationId": conv.id, "snoozedUntil": snoozed_until }),
        Some(conv.id),
        None,
    );
    let _ = conn.execute(
        "UPDATE conversations SET snoozed_until = ?1,
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id = ?2",
        params![snoozed_until, conv.id],
    );
    set_outbound_status(conn, job_id, "confirmed", None);
    let _ = crate::jobs::audit(
        conn,
        "user",
        "snoozed",
        Some(conv.id),
        None,
        Some(&json!({ "snoozedUntil": snoozed_until }).to_string()),
        Some(&format!("PUT /v2/conversations/{}/snooze", conv.remote_id)),
        Some(&json!({ "job_id": job_id }).to_string()),
        false,
    );
    ok_result(&format!("Snoozed until {snoozed_until}."), None)
}

pub fn op_unsnooze(conn: &Connection, conversation_local_id: i64) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation not found locally.");
    };
    let _ = conn.execute(
        "UPDATE conversations SET snoozed_until = NULL,
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id = ?1",
        params![conv.id],
    );
    let _ = crate::jobs::audit(
        conn,
        "user",
        "unsnoozed",
        Some(conv.id),
        None,
        None,
        Some(&format!(
            "DELETE /v2/conversations/{}/snooze",
            conv.remote_id
        )),
        None,
        false,
    );
    ok_result("Snooze removed.", None)
}

// ---------------------------------------------------------------------------
// scheduled replies (operations.ts:383-436)
// ---------------------------------------------------------------------------

struct ThreadRef {
    #[allow(dead_code)]
    id: i64,
    conversation_id: i64,
}

fn thread_by_local_id(conn: &Connection, thread_id: i64) -> Option<ThreadRef> {
    conn.query_row(
        "SELECT id, conversation_id FROM conversation_threads WHERE id = ?1",
        params![thread_id],
        |r| {
            Ok(ThreadRef {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
            })
        },
    )
    .ok()
}

pub fn op_schedule_reply(
    conn: &Connection,
    conversation_local_id: i64,
    thread_id: i64,
    scheduled_for: &str,
) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation or draft thread not found locally.");
    };
    let Some(thread) = thread_by_local_id(conn, thread_id) else {
        return rejected("Conversation or draft thread not found locally.");
    };
    if thread.conversation_id != conv.id {
        return rejected("That thread does not belong to this conversation.");
    }
    let job_id = create_outbound_job(
        conn,
        "schedule",
        &json!({ "conversationId": conv.id, "threadId": thread_id, "scheduledFor": scheduled_for }),
        Some(conv.id),
        Some(thread_id),
    );
    let _ = conn.execute(
        "UPDATE conversation_threads SET scheduled_for = ?1, state = 'scheduled'
           WHERE id = ?2",
        params![scheduled_for, thread_id],
    );
    set_outbound_status(conn, job_id, "confirmed", None);
    let _ = crate::jobs::audit(
        conn,
        "user",
        "reply_scheduled",
        Some(conv.id),
        None,
        Some(&json!({ "scheduledFor": scheduled_for }).to_string()),
        Some(&format!(
            "PUT /v2/conversations/{}/threads/{thread_id}/schedule",
            conv.remote_id
        )),
        Some(&json!({ "job_id": job_id }).to_string()),
        false,
    );
    ok_result(&format!("Reply scheduled for {scheduled_for}."), None)
}

pub fn op_publish_schedule(
    conn: &Connection,
    conversation_local_id: i64,
    thread_id: i64,
) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation or thread not found locally.");
    };
    let Some(thread) = thread_by_local_id(conn, thread_id) else {
        return rejected("Conversation or thread not found locally.");
    };
    if thread.conversation_id != conv.id {
        return rejected("That thread does not belong to this conversation.");
    }
    let job_id = create_outbound_job(
        conn,
        "schedule_publish",
        &json!({ "conversationId": conv.id, "threadId": thread_id }),
        Some(conv.id),
        Some(thread_id),
    );
    let _ = conn.execute(
        "UPDATE conversation_threads SET state = 'published', scheduled_for = NULL
           WHERE id = ?1",
        params![thread_id],
    );
    set_outbound_status(conn, job_id, "confirmed", None);
    let _ = crate::jobs::audit(
        conn,
        "user",
        "scheduled_reply_published",
        Some(conv.id),
        None,
        None,
        Some("PATCH .../schedule (publish)"),
        Some(&json!({ "job_id": job_id }).to_string()),
        false,
    );
    ok_result("Scheduled reply published (sent now).", None)
}

pub fn op_delete_schedule(
    conn: &Connection,
    conversation_local_id: i64,
    thread_id: i64,
) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation or thread not found locally.");
    };
    let Some(thread) = thread_by_local_id(conn, thread_id) else {
        return rejected("Conversation or thread not found locally.");
    };
    if thread.conversation_id != conv.id {
        return rejected("That thread does not belong to this conversation.");
    }
    let _ = conn.execute(
        "UPDATE conversation_threads SET state = 'draft', scheduled_for = NULL
           WHERE id = ?1",
        params![thread_id],
    );
    let _ = crate::jobs::audit(
        conn,
        "user",
        "schedule_deleted",
        Some(conv.id),
        None,
        None,
        Some("DELETE .../schedule"),
        None,
        false,
    );
    ok_result("Schedule deleted; draft kept.", None)
}

// ---------------------------------------------------------------------------
// bulk actions (operations.ts:429 — queue-based, spec #95)
// ---------------------------------------------------------------------------

/// Queue-based bulk actions: one job per conversation, priority 1, the
/// reference's exact `bulk_{action}` types.
pub fn op_bulk_action(
    conn: &Connection,
    conversation_ids: &[i64],
    action: &str,
    params_body: &Value,
) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let mut queued = 0usize;
    for id in conversation_ids {
        let mut payload = json!({ "conversationId": id });
        if let Some(obj) = params_body.as_object() {
            for (k, v) in obj {
                payload[k] = v.clone();
            }
        }
        let payload_str = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
        let _ = crate::jobs::enqueue_on(conn, "api", &format!("bulk_{action}"), &payload_str, 1, 3);
        queued += 1;
    }
    let _ = crate::jobs::audit(
        conn,
        "user",
        &format!("bulk_{action}"),
        None,
        Some(&json!({ "count": conversation_ids.len() }).to_string()),
        Some(&json!({ "queued": queued }).to_string()),
        None,
        None,
        false,
    );
    ok_result(
        &format!("{queued} operations queued."),
        Some(json!({ "queued": queued })),
    )
}

// ---------------------------------------------------------------------------
// Help Scout workflow (operations.ts:refreshOne-adjacent)
// ---------------------------------------------------------------------------

pub fn op_run_workflow(
    conn: &Connection,
    conversation_local_id: i64,
    workflow_id: i64,
) -> Response {
    if let Some(msg) = eval_mode_blocked(conn) {
        return rejected(msg);
    }
    let Some(conv) = conv_by_local_id(conn, conversation_local_id) else {
        return rejected("Conversation not found locally.");
    };
    // Demo semantics (fakeProvider.runWorkflow): a workflow whose name
    // contains "Tier 1" assigns user 1001; everything else is a no-op that
    // still succeeds.
    let wf_name: Option<String> = conn
        .query_row(
            "SELECT name FROM workflows WHERE remote_id = ?1",
            params![workflow_id],
            |r| r.get(0),
        )
        .ok();
    if let Some(name) = wf_name.as_deref() {
        if name.contains("Tier 1") {
            let _ = conn.execute(
                "UPDATE conversations SET assignee_id =
                     (SELECT id FROM users WHERE remote_id = 1001)
                   WHERE id = ?1",
                params![conv.id],
            );
        }
    }
    let _ = crate::jobs::audit(
        conn,
        "user",
        "helpscout_workflow_run",
        Some(conv.id),
        None,
        None,
        Some(&format!("POST /v2/workflows/{workflow_id}/run")),
        None,
        false,
    );
    ok_result("Help Scout workflow executed.", None)
}

// ---------------------------------------------------------------------------
// Attachments (operations.ts:downloadAttachment + system.ts:261)
// ---------------------------------------------------------------------------

pub struct AttachmentRow {
    pub id: i64,
    pub remote_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub conversation_id: i64,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub local_path: Option<String>,
    pub state: Option<String>,
}

pub fn attachment_by_id(conn: &Connection, id: i64) -> Option<AttachmentRow> {
    conn.query_row(
        "SELECT id, remote_id, thread_id, conversation_id, filename, mime_type,
                local_path, state
           FROM attachments WHERE id = ?1",
        params![id],
        |r| {
            Ok(AttachmentRow {
                id: r.get(0)?,
                remote_id: r.get(1)?,
                thread_id: r.get(2)?,
                conversation_id: r.get(3)?,
                filename: r.get(4)?,
                mime_type: r.get(5)?,
                local_path: r.get(6)?,
                state: r.get(7)?,
            })
        },
    )
    .ok()
}

/// Attachment download (operations.ts downloadAttachment:537-561): fetch
/// the bytes through the PROVIDER boundary (SY-10 — the real provider hits
/// `/v2/conversations/:id/attachments/:aid/data`, the fake serves simulated
/// content), then persist with the reference's
/// `{conversationId}-{attachmentId}-{safeName}` layout + sha256. A provider
/// `None` marks the attachment failed ("no longer available").
pub async fn op_download_attachment(
    state: &crate::http::server::AppState,
    attachments_dir: &std::path::Path,
    attachment_id: i64,
) -> Response {
    let att = {
        let conn = state.conn_lock();
        attachment_by_id(&conn, attachment_id)
    };
    let Some(att) = att else {
        return rejected("Attachment not found.");
    };
    // Parent conversation + its remote id (the provider keys on remote ids).
    let (conv_ok, conv_remote, thread_remote): (bool, i64, Option<i64>) = {
        let conn = state.conn_lock();
        let conv = conv_by_local_id(&conn, att.conversation_id);
        let t_remote: Option<i64> = att
            .thread_id
            .and_then(|tid| {
                conn.query_row(
                    "SELECT remote_id FROM conversation_threads WHERE id = ?1",
                    params![tid],
                    |r| r.get(0),
                )
                .ok()
            })
            .flatten();
        (
            conv.is_some(),
            conv.map(|c| c.remote_id).unwrap_or_default(),
            t_remote,
        )
    };
    if !conv_ok {
        return rejected("Parent conversation not found.");
    }
    let Some(provider) = op_provider(state) else {
        return internal_error("no provider available");
    };
    match provider
        .get_attachment_data(
            conv_remote,
            thread_remote.unwrap_or(0),
            att.remote_id.unwrap_or(0),
        )
        .await
    {
        Ok(Some(bytes)) => {
            let conn = state.conn_lock();
            match persist_attachment_bytes(&conn, attachments_dir, attachment_id, &bytes.data) {
                Ok(path) => ok_result("Attachment downloaded.", Some(json!({ "path": path }))),
                Err(msg) => rejected(&msg),
            }
        }
        Ok(None) => {
            let conn = state.conn_lock();
            set_attachment_state(&conn, attachment_id, "failed", None, None);
            rejected(
                "Attachment is no longer available in Help Scout (it may have expired or been removed).",
            )
        }
        Err(e) => {
            let conn = state.conn_lock();
            set_attachment_state(&conn, attachment_id, "failed", None, None);
            rejected(&format!(
                "Attachment download failed. {}",
                failure_message(&e)
            ))
        }
    }
}

/// Persist downloaded attachment bytes: the reference's
/// `{conversationId}-{attachmentId}-{safeName}` layout, sha256 + the
/// `downloaded` state flip (the write-to-disk half of the old
/// `download_attachment_to`).
pub fn persist_attachment_bytes(
    conn: &Connection,
    attachments_dir: &std::path::Path,
    attachment_id: i64,
    data: &[u8],
) -> std::result::Result<String, String> {
    let Some(att) = attachment_by_id(conn, attachment_id) else {
        return Err("Attachment not found.".into());
    };
    if conv_by_local_id(conn, att.conversation_id).is_none() {
        return Err("Parent conversation not found.".into());
    }
    let safe_name = att
        .filename
        .as_deref()
        .unwrap_or("attachment")
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    let _ = std::fs::create_dir_all(attachments_dir);
    let target = attachments_dir.join(format!(
        "{}-{}-{}",
        att.conversation_id, attachment_id, safe_name
    ));
    if let Err(e) = std::fs::write(&target, data) {
        set_attachment_state(conn, attachment_id, "failed", None, None);
        return Err(format!("Attachment download failed. {e}"));
    }
    use sha2::{Digest, Sha256};
    let hash = hex(&Sha256::digest(data));
    let path_str = target.to_string_lossy().to_string();
    set_attachment_state(
        conn,
        attachment_id,
        "downloaded",
        Some(&path_str),
        Some(&hash),
    );
    Ok(path_str)
}

/// Worker-path attachment download (WK-03 + SY-10): fetch the bytes through
/// the provider (fake serves simulated content; the real provider decodes
/// the v2 wire data — real-provider rows no longer stay pending), then
/// persist. Shared by the `download_recent_attachments` job; the route uses
/// the full [`op_download_attachment`] pipeline.
pub async fn download_attachment_via_provider(
    state: &crate::http::server::AppState,
    attachments_dir: &std::path::Path,
    attachment_id: i64,
) -> std::result::Result<String, String> {
    let att = {
        let conn = state.conn_lock();
        attachment_by_id(&conn, attachment_id)
    };
    let Some(att) = att else {
        return Err("Attachment not found.".into());
    };
    let (conv_remote, thread_remote): (i64, Option<i64>) = {
        let conn = state.conn_lock();
        let conv = conv_by_local_id(&conn, att.conversation_id)
            .map(|c| c.remote_id)
            .unwrap_or_default();
        let t_remote: Option<i64> = att
            .thread_id
            .and_then(|tid| {
                conn.query_row(
                    "SELECT remote_id FROM conversation_threads WHERE id = ?1",
                    params![tid],
                    |r| r.get(0),
                )
                .ok()
            })
            .flatten();
        (conv, t_remote)
    };
    let Some(provider) = op_provider(state) else {
        return Err("no provider available".into());
    };
    match provider
        .get_attachment_data(
            conv_remote,
            thread_remote.unwrap_or(0),
            att.remote_id.unwrap_or(0),
        )
        .await
    {
        Ok(Some(bytes)) => {
            let conn = state.conn_lock();
            persist_attachment_bytes(&conn, attachments_dir, attachment_id, &bytes.data)
        }
        Ok(None) => {
            let conn = state.conn_lock();
            set_attachment_state(&conn, attachment_id, "failed", None, None);
            Err("Attachment is no longer available in Help Scout.".into())
        }
        Err(e) => {
            let conn = state.conn_lock();
            set_attachment_state(&conn, attachment_id, "failed", None, None);
            Err(format!(
                "Attachment download failed. {}",
                failure_message(&e)
            ))
        }
    }
}

pub fn set_attachment_state(
    conn: &Connection,
    id: i64,
    state: &str,
    path: Option<&str>,
    hash: Option<&str>,
) {
    let _ = conn.execute(
        "UPDATE attachments SET state = ?1, local_path = COALESCE(?2, local_path),
             hash = COALESCE(?3, hash), downloaded_at = datetime('now')
           WHERE id = ?4",
        params![state, path, hash, id],
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Reply / note / status / assign / subject (operations.ts:69-237)
//
// The FULL write pipeline: validate (route) → auth → fresh-read →
// idempotency → job → provider write → confirm → persist/refresh → audit.
// Replies are idempotency-keyed and NEVER auto-retried; demo-mode writes go
// through the same provider boundary as real mode (the fake world IS the
// remote), then persist via the single-conversation refresh — so the world
// and the local mirror can never disagree.
// ---------------------------------------------------------------------------

use std::time::Instant;

use crate::helpscout::{ConversationPatch, CreateThreadInput, HelpScoutProvider};

/// Whether evaluation mode is ON (the shared flag check — each op carries
/// its own reference message).
pub fn eval_mode_on(conn: &Connection) -> bool {
    let on: Option<String> = crate::settings::get_string(conn, "ai_evaluation_mode")
        .ok()
        .flatten();
    matches!(on.as_deref(), Some("1") | Some("true") | Some("on"))
}

/// Full conversation row the write pipeline needs (fresh read).
pub struct ConvFull {
    pub id: i64,
    pub remote_id: i64,
    pub status: String,
    pub subject: Option<String>,
    pub closed_at: Option<String>,
    pub assignee_local_id: Option<i64>,
    pub merged_into_conversation_id: Option<i64>,
}

/// `getConversationByLocalId` — the ops work on the LOCAL id and require a
/// remote mapping (operations.ts conv checks).
pub fn conv_full_by_local_id(conn: &Connection, id: i64) -> Option<ConvFull> {
    conn.query_row(
        "SELECT id, remote_id, status, subject, closed_at, assignee_id,
                merged_into_conversation_id
           FROM conversations WHERE id = ?1",
        params![id],
        |r| {
            Ok(ConvFull {
                id: r.get(0)?,
                remote_id: r.get(1)?,
                status: r.get(2)?,
                subject: r.get(3)?,
                closed_at: r.get(4)?,
                assignee_local_id: r.get(5)?,
                merged_into_conversation_id: r.get(6)?,
            })
        },
    )
    .ok()
}

/// `requireAuth` (operations.ts:48) — demo mode is always "authenticated";
/// real mode needs a live OAuth token.
pub const NOT_CONNECTED: &str = "Help Scout is not connected. Remote actions are disabled - connect Help Scout in Settings first.";

pub fn require_auth(conn: &Connection, provider_kind: &str) -> Option<&'static str> {
    if provider_kind != "real" {
        return None;
    }
    // The token table grew `account` + `revoked` columns for the
    // reference-shaped store; older DBs only have the id=1 row.
    let has_cols: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('oauth_tokens')
              WHERE name IN ('account', 'revoked')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n == 2)
        .unwrap_or(false);
    let connected = if has_cols {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM oauth_tokens
               WHERE (account = 'default' OR id = 1)
                 AND access_token IS NOT NULL AND access_token != ''
                 AND COALESCE(revoked, 0) = 0)",
            [],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false)
    } else {
        crate::oauth::has_token(conn).unwrap_or(false)
    };
    if connected {
        None
    } else {
        Some(NOT_CONNECTED)
    }
}

/// `createOutboundJob(kind, payload, opts)` with the idempotency key
/// (jobRepo.ts:167). A retry after a FAILED job reuses the same key: the
/// row is reset instead of hitting the UNIQUE constraint (the reference
/// crashes there — latent bug, reported).
pub fn create_outbound_job_keyed(
    conn: &Connection,
    kind: &str,
    payload: &Value,
    conversation_id: Option<i64>,
    idempotency_key: Option<&str>,
) -> i64 {
    let payload = serde_json::to_string(payload).unwrap_or_else(|_| "{}".into());
    let res = conn.execute(
        "INSERT INTO outbound_jobs (kind, conversation_id, thread_id, payload,
             status, requires_confirmation, idempotency_key, created_at, updated_at)
         VALUES (?1, ?2, NULL, ?3, 'queued', 0, ?4, datetime('now'), datetime('now'))
         ON CONFLICT(idempotency_key) DO UPDATE SET
            kind = excluded.kind, payload = excluded.payload,
            status = 'queued', error = NULL, remote_result = NULL,
            attempts = 0, updated_at = datetime('now')",
        params![kind, conversation_id, payload, idempotency_key],
    );
    if res.is_err() {
        return 0;
    }
    // ON CONFLICT(...) DO UPDATE keeps the existing row's id.
    conn.query_row(
        "SELECT id FROM outbound_jobs WHERE idempotency_key = ?1",
        params![idempotency_key],
        |r| r.get(0),
    )
    .unwrap_or_else(|_| conn.last_insert_rowid())
}

/// `sha256(text).digest('hex').slice(0, 24)` — the reply idempotency hash.
fn sha256_24(text: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(text.as_bytes());
    hex(&digest)[..24].to_string()
}

/// `{ok:false}` with 503 — the reference reply route maps a "not connected"
/// message to Service Unavailable.
fn not_connected(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "ok": false, "message": message })),
    )
        .into_response()
}

/// The provider the ops write through (one instance with the sync engine —
/// reference AppContext binding).
pub fn op_provider(
    state: &crate::http::server::AppState,
) -> Option<std::sync::Arc<dyn HelpScoutProvider>> {
    if let Some(sync) = &state.sync {
        return Some(sync.provider().clone());
    }
    state
        .real
        .clone()
        .map(|r| r as std::sync::Arc<dyn HelpScoutProvider>)
}

/// The message the failure path shows: the friendly Help Scout text when the
/// error is an API error, else the raw error (operations.ts catch).
fn failure_message(e: &crate::error::Error) -> String {
    crate::helpscout_real::hs_error(e)
        .map(|hs| hs.friendly.clone())
        .unwrap_or_else(|| e.to_string())
}

/// `detail` for the failure path: `HTTP {status}` (+ correlation when real).
fn failure_detail(e: &crate::error::Error) -> Option<String> {
    crate::helpscout_real::hs_error(e).map(|hs| format!("HTTP {}", hs.status_code))
}

/// Validated reply request (replyRequestSchema + the AI-04 send-provenance
/// pair the reference's sendReply accepts, operations.ts:69).
#[derive(Debug, Clone)]
pub struct ReplyInput {
    pub conversation_id: i64,
    pub text: String,
    pub draft: bool,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub status_after: Option<String>,
    pub assign_to: Option<i64>,
    /// The AI draft this reply was generated from — marks the draft 'sent',
    /// records was_sent feedback and flips the audit row's ai_involvement.
    pub ai_draft_id: Option<i64>,
    /// The draft's original AI text (edit-distance baseline).
    pub original_ai_text: Option<String>,
}

/// `sendReply` (operations.ts:69).
pub async fn op_send_reply(state: &crate::http::server::AppState, input: ReplyInput) -> Response {
    // auth → eval → fresh read → merged guard → idempotency → job
    let (conv, job_id) = {
        let conn = state.conn_lock();
        if let Some(msg) = require_auth(&conn, &state.provider_kind) {
            return not_connected(msg);
        }
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        let Some(conv) = conv_full_by_local_id(&conn, input.conversation_id) else {
            return rejected("Conversation not found locally.");
        };
        if conv.merged_into_conversation_id.is_some() {
            return rejected(
                "This conversation was merged into another conversation. Open the target conversation to reply.",
            );
        }
        // The draft flag is part of the key: saving a draft of text T and
        // then SENDING the same text T is the normal draft-then-send flow,
        // not a duplicate send.
        let key = format!(
            "reply:{}:{}:{}",
            conv.remote_id,
            if input.draft { "draft" } else { "send" },
            sha256_24(&input.text)
        );
        let existing: Option<String> = conn
            .query_row(
                "SELECT status FROM outbound_jobs WHERE idempotency_key = ?1",
                params![&key],
                |r| r.get(0),
            )
            .ok();
        if existing.is_some_and(|status| status != "failed") {
            return rejected(
                "This exact reply was already sent (duplicate-send protection). Check the conversation history before sending again.",
            );
        }
        let job_id = create_outbound_job_keyed(
            &conn,
            "create_reply",
            &json!({
                "conversationId": input.conversation_id,
                "text": input.text,
                "draft": input.draft,
                "cc": input.cc,
                "bcc": input.bcc,
                "statusAfter": input.status_after,
                "assignTo": input.assign_to,
            }),
            Some(conv.id),
            Some(&key),
        );
        set_outbound_status(&conn, job_id, "sending", None);
        (conv, job_id)
    };

    let Some(provider) = op_provider(state) else {
        return internal_error("no provider available");
    };
    let started = Instant::now();
    match provider
        .create_reply_thread(CreateThreadInput {
            conversation_id: conv.remote_id,
            text: input.text.clone(),
            draft: input.draft,
            cc: input.cc.clone(),
            bcc: input.bcc.clone(),
            status_after: input.status_after.clone(),
            assign_to: input.assign_to,
        })
        .await
    {
        Ok(res) => {
            let elapsed = started.elapsed().as_millis() as i64;
            {
                let conn = state.conn_lock();
                let _ = crate::jobs::record_outbound_attempt(
                    &conn,
                    job_id,
                    1,
                    &format!("POST reply (draft={})", input.draft),
                    Some(201),
                    None,
                    Some(elapsed),
                );
                set_outbound_status_confirmed(
                    &conn,
                    job_id,
                    &json!({ "threadId": res.thread_id, "conversationId": res.conversation_id }),
                );
            }
            // Persist locally after the confirmed remote write (refreshOne).
            if let Some(sync) = &state.sync {
                let _ = sync.sync_single_conversation(conv.remote_id).await;
            }
            // AI-04 (draft send provenance — reference operations.ts:105-107):
            // a reply that rode an AI draft marks the draft 'sent' and
            // records the was_sent feedback (edit distance between the
            // original AI text and the final sent text).
            if let Some(draft_id) = input.ai_draft_id {
                let conn = state.conn_lock();
                let _ = crate::ai_pipeline::ensure_pipeline_schema(&conn);
                let _ = crate::ai_pipeline::set_draft_state(&conn, draft_id, "sent");
                let _ = crate::ai_pipeline::record_feedback(
                    &conn,
                    draft_id,
                    input.original_ai_text.as_deref().unwrap_or(""),
                    &input.text,
                    true,
                );
            }
            {
                let conn = state.conn_lock();
                let _ = crate::jobs::audit_entry(
                    &conn,
                    "user",
                    if input.draft {
                        "reply_draft_created"
                    } else {
                        "reply_sent"
                    },
                    Some(conv.id),
                    Some(&json!({ "status": conv.status }).to_string()),
                    Some(&json!({ "threadRemoteId": res.thread_id }).to_string()),
                    Some(&format!("POST /v2/conversations/{}/reply", conv.remote_id)),
                    Some(
                        &json!({
                            "threadId": res.thread_id,
                            "conversationId": res.conversation_id
                        })
                        .to_string(),
                    ),
                    input.ai_draft_id.is_some(),
                    Some(job_id),
                    None,
                );
            }
            emit_conversation_updated(state, conv.id);
            ok_result(
                if input.draft {
                    "Draft saved to Help Scout."
                } else {
                    "Reply sent successfully."
                },
                Some(json!({ "threadRemoteId": res.thread_id })),
            )
        }
        Err(e) => {
            let msg = failure_message(&e);
            let detail = failure_detail(&e);
            {
                let conn = state.conn_lock();
                let _ = crate::jobs::record_outbound_attempt(
                    &conn,
                    job_id,
                    1,
                    "POST reply",
                    crate::helpscout_real::hs_status(&e).map(i64::from),
                    Some(&e.to_string()),
                    Some(started.elapsed().as_millis() as i64),
                );
                set_outbound_status(&conn, job_id, "failed", Some(&msg));
                let _ = crate::jobs::audit_entry(
                    &conn,
                    "user",
                    "reply_failed",
                    Some(conv.id),
                    Some(&json!({ "status": conv.status }).to_string()),
                    None,
                    Some(&format!("POST /v2/conversations/{}/reply", conv.remote_id)),
                    Some(&json!({ "error": msg }).to_string()),
                    false,
                    Some(job_id),
                    None,
                );
            }
            // Replies are NEVER auto-retried: a timeout may still have
            // delivered the message remotely.
            let caution = if input.draft {
                String::new()
            } else {
                " The reply may or may not have been delivered - verify in Help Scout before sending again; SupportOS will not automatically resend.".to_string()
            };
            let mut body = json!({ "ok": false, "message": format!("{msg}{caution}") });
            if let Some(d) = detail {
                body["detail"] = json!(d);
            }
            (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response()
        }
    }
}

/// `addNote` (operations.ts:133).
pub async fn op_add_note(
    state: &crate::http::server::AppState,
    conversation_id: i64,
    text: &str,
    ai_generated: bool,
) -> Response {
    let (conv, job_id) = {
        let conn = state.conn_lock();
        if let Some(msg) = require_auth(&conn, &state.provider_kind) {
            return not_connected(msg);
        }
        let Some(conv) = conv_full_by_local_id(&conn, conversation_id) else {
            return rejected("Conversation not found locally.");
        };
        if eval_mode_on(&conn) {
            // operations.ts addNote carries its own eval message.
            return rejected(
                "AI evaluation mode is ON: no notes, replies or status changes are sent to Help Scout.",
            );
        }
        let job_id = create_outbound_job_keyed(
            &conn,
            "create_note",
            &json!({ "conversationId": conversation_id, "text": text, "aiGenerated": ai_generated }),
            Some(conv.id),
            None,
        );
        set_outbound_status(&conn, job_id, "sending", None);
        (conv, job_id)
    };

    let Some(provider) = op_provider(state) else {
        return internal_error("no provider available");
    };
    let started = Instant::now();
    let attempt = provider.create_note_thread(CreateThreadInput {
        conversation_id: conv.remote_id,
        text: text.to_string(),
        draft: false,
        cc: vec![],
        bcc: vec![],
        status_after: None,
        assign_to: None,
    });
    match attempt.await {
        Ok(res) => {
            let elapsed = started.elapsed().as_millis() as i64;
            {
                let conn = state.conn_lock();
                let _ = crate::jobs::record_outbound_attempt(
                    &conn,
                    job_id,
                    1,
                    "POST note",
                    Some(201),
                    None,
                    Some(elapsed),
                );
                set_outbound_status_confirmed(
                    &conn,
                    job_id,
                    &json!({ "threadId": res.thread_id, "conversationId": res.conversation_id }),
                );
            }
            if let Some(sync) = &state.sync {
                let _ = sync.sync_single_conversation(conv.remote_id).await;
            }
            {
                let conn = state.conn_lock();
                let _ = crate::jobs::audit_entry(
                    &conn,
                    if ai_generated { "ai" } else { "user" },
                    "note_added",
                    Some(conv.id),
                    None,
                    None,
                    Some(&format!("POST /v2/conversations/{}/notes", conv.remote_id)),
                    Some(
                        &json!({
                            "threadId": res.thread_id,
                            "conversationId": res.conversation_id
                        })
                        .to_string(),
                    ),
                    ai_generated,
                    Some(job_id),
                    None,
                );
            }
            emit_conversation_updated(state, conv.id);
            ok_result(
                "Internal note added.",
                Some(json!({ "threadRemoteId": res.thread_id })),
            )
        }
        Err(e) => {
            let msg = failure_message(&e);
            let detail = failure_detail(&e);
            {
                let conn = state.conn_lock();
                let _ = crate::jobs::record_outbound_attempt(
                    &conn,
                    job_id,
                    1,
                    "POST note",
                    crate::helpscout_real::hs_status(&e).map(i64::from),
                    Some(&e.to_string()),
                    Some(started.elapsed().as_millis() as i64),
                );
                set_outbound_status(&conn, job_id, "failed", Some(&msg));
            }
            let mut body = json!({ "ok": false, "message": msg });
            if let Some(d) = detail {
                body["detail"] = json!(d);
            }
            (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response()
        }
    }
}

/// `changeStatus` (operations.ts:170) — status write + closed_at stamp are
/// ONE atomic local transaction (v2.2.1 audit fix).
pub async fn op_change_status(
    state: &crate::http::server::AppState,
    conversation_id: i64,
    status: &str,
) -> Response {
    let conv = {
        let conn = state.conn_lock();
        if require_auth(&conn, &state.provider_kind).is_some() {
            // Note/status/assign/subject routes keep 422 for not-connected
            // (only the reply route maps it to 503).
            return rejected(NOT_CONNECTED);
        }
        if eval_mode_on(&conn) {
            // operations.ts changeStatus carries its own eval message.
            return rejected("AI evaluation mode is ON: status changes are disabled.");
        }
        let Some(conv) = conv_full_by_local_id(&conn, conversation_id) else {
            return rejected("Conversation not found locally.");
        };
        conv
    };
    let Some(provider) = op_provider(state) else {
        return internal_error("no provider available");
    };
    let job_id = {
        let conn = state.conn_lock();
        let job_id = create_outbound_job_keyed(
            &conn,
            "update_status",
            &json!({ "conversationId": conversation_id, "status": status }),
            Some(conv.id),
            None,
        );
        set_outbound_status(&conn, job_id, "sending", None);
        job_id
    };
    match provider
        .update_conversation(
            conv.remote_id,
            ConversationPatch {
                status: Some(status.to_string()),
                ..Default::default()
            },
        )
        .await
    {
        Ok(_) => {
            let mut conn = state.conn_lock();
            set_outbound_status(&conn, job_id, "confirmed", None);
            // Atomic: status write + closed_at stamp together. C2 (T16
            // audit): every statement is CHECKED — a DB error must surface
            // as the 500 envelope, never as ok:true + SSE (fake success).
            // A failed statement returns early, dropping the uncommitted
            // transaction (automatic rollback).
            let tx = match conn.transaction() {
                Ok(tx) => tx,
                Err(e) => return internal_error(&e.to_string()),
            };
            if let Err(e) = tx.execute(
                "UPDATE conversations SET status = ?1,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                   WHERE id = ?2",
                params![status, conv.id],
            ) {
                return internal_error(&e.to_string());
            }
            if status == "closed" && conv.closed_at.is_none() {
                if let Err(e) = tx.execute(
                    "UPDATE conversations SET closed_at = datetime('now') WHERE id = ?1",
                    params![conv.id],
                ) {
                    return internal_error(&e.to_string());
                }
            }
            if let Err(e) = tx.commit() {
                return internal_error(&e.to_string());
            }
            let _ = crate::jobs::audit_entry(
                &conn,
                "user",
                "status_changed",
                Some(conv.id),
                Some(&json!({ "status": conv.status }).to_string()),
                Some(&json!({ "status": status }).to_string()),
                Some(&format!("PATCH /v2/conversations/{}", conv.remote_id)),
                None,
                false,
                Some(job_id),
                None,
            );
            drop(conn);
            emit_conversation_updated(state, conv.id);
            ok_result(&format!("Status changed to {status}."), None)
        }
        Err(e) => {
            let msg = failure_message(&e);
            let conn = state.conn_lock();
            set_outbound_status(&conn, job_id, "failed", Some(&msg));
            drop(conn);
            rejected(&format!("Status was NOT changed. {msg}"))
        }
    }
}

/// `assign` (operations.ts:201) — userId is a REMOTE user/team id (or null
/// to unassign); the local persist resolves it through the mirror.
pub async fn op_assign(
    state: &crate::http::server::AppState,
    conversation_id: i64,
    user_id: Option<i64>,
) -> Response {
    let conv = {
        let conn = state.conn_lock();
        if require_auth(&conn, &state.provider_kind).is_some() {
            return rejected(NOT_CONNECTED);
        }
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        let Some(conv) = conv_full_by_local_id(&conn, conversation_id) else {
            return rejected("Conversation not found locally.");
        };
        conv
    };
    let Some(provider) = op_provider(state) else {
        return internal_error("no provider available");
    };
    let job_id = {
        let conn = state.conn_lock();
        let job_id = create_outbound_job_keyed(
            &conn,
            "assign",
            &json!({ "conversationId": conversation_id, "userId": user_id }),
            Some(conv.id),
            None,
        );
        set_outbound_status(&conn, job_id, "sending", None);
        job_id
    };
    match provider
        .update_conversation(
            conv.remote_id,
            ConversationPatch {
                assign_to: Some(user_id),
                ..Default::default()
            },
        )
        .await
    {
        Ok(_) => {
            let conn = state.conn_lock();
            set_outbound_status(&conn, job_id, "confirmed", None);
            // Resolve the remote id to the local user (or team) row.
            let local_user: Option<i64> = user_id.and_then(|uid| {
                conn.query_row(
                    "SELECT id FROM users WHERE remote_id = ?1",
                    params![uid],
                    |r| r.get(0),
                )
                .ok()
            });
            let local_team: Option<i64> = match (user_id, local_user) {
                (Some(uid), None) => conn
                    .query_row(
                        "SELECT id FROM teams WHERE remote_id = ?1",
                        params![uid],
                        |r| r.get(0),
                    )
                    .ok(),
                _ => None,
            };
            let resolved = local_user.or(local_team).or(user_id);
            let _ = conn.execute(
                "UPDATE conversations SET assignee_id = ?1,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                   WHERE id = ?2",
                params![resolved, conv.id],
            );
            let _ = crate::jobs::audit_entry(
                &conn,
                "user",
                "assignment_changed",
                Some(conv.id),
                Some(&json!({ "assignee": conv.assignee_local_id }).to_string()),
                Some(&json!({ "assignee": user_id }).to_string()),
                Some(&format!(
                    "PATCH /v2/conversations/{} /assignTo",
                    conv.remote_id
                )),
                None,
                false,
                Some(job_id),
                None,
            );
            drop(conn);
            emit_conversation_updated(state, conv.id);
            ok_result(
                if user_id.is_some() {
                    "Conversation assigned."
                } else {
                    "Conversation unassigned."
                },
                None,
            )
        }
        Err(e) => {
            let msg = failure_message(&e);
            let conn = state.conn_lock();
            set_outbound_status(&conn, job_id, "failed", Some(&msg));
            drop(conn);
            rejected(&format!("Assignment was NOT changed. {msg}"))
        }
    }
}

/// `changeSubject` (operations.ts:226).
pub async fn op_change_subject(
    state: &crate::http::server::AppState,
    conversation_id: i64,
    subject: &str,
) -> Response {
    let conv = {
        let conn = state.conn_lock();
        if require_auth(&conn, &state.provider_kind).is_some() {
            return rejected(NOT_CONNECTED);
        }
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        let Some(conv) = conv_full_by_local_id(&conn, conversation_id) else {
            return rejected("Conversation not found locally.");
        };
        conv
    };
    let Some(provider) = op_provider(state) else {
        return internal_error("no provider available");
    };
    let job_id = {
        let conn = state.conn_lock();
        let job_id = create_outbound_job_keyed(
            &conn,
            "update_subject",
            &json!({ "conversationId": conversation_id, "subject": subject }),
            Some(conv.id),
            None,
        );
        set_outbound_status(&conn, job_id, "sending", None);
        job_id
    };
    match provider
        .update_conversation(
            conv.remote_id,
            ConversationPatch {
                subject: Some(subject.to_string()),
                ..Default::default()
            },
        )
        .await
    {
        Ok(_) => {
            let conn = state.conn_lock();
            set_outbound_status(&conn, job_id, "confirmed", None);
            let _ = conn.execute(
                "UPDATE conversations SET subject = ?1,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                   WHERE id = ?2",
                params![subject, conv.id],
            );
            let _ = crate::jobs::audit_entry(
                &conn,
                "user",
                "subject_changed",
                Some(conv.id),
                Some(&json!({ "subject": conv.subject }).to_string()),
                Some(&json!({ "subject": subject }).to_string()),
                Some(&format!(
                    "PATCH /v2/conversations/{} /subject",
                    conv.remote_id
                )),
                None,
                false,
                Some(job_id),
                None,
            );
            drop(conn);
            emit_conversation_updated(state, conv.id);
            ok_result("Subject updated.", None)
        }
        Err(e) => {
            let msg = failure_message(&e);
            let conn = state.conn_lock();
            set_outbound_status(&conn, job_id, "failed", Some(&msg));
            drop(conn);
            rejected(&format!("Subject was NOT changed. {msg}"))
        }
    }
}

/// `setOutboundStatus(id, status, error?, remoteResult?)` — the confirmed
/// flavor also persists the remote result JSON.
fn set_outbound_status_confirmed(conn: &Connection, id: i64, remote_result: &Value) {
    if id == 0 {
        return;
    }
    let _ = conn.execute(
        "UPDATE outbound_jobs
            SET status = 'confirmed', error = NULL, remote_result = ?1,
                attempts = attempts + 1, updated_at = datetime('now')
          WHERE id = ?2",
        params![remote_result.to_string(), id],
    );
}

/// Standard 500 (should be unreachable in a wired app).
fn internal_error(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "statusCode": 500,
            "error": "InternalError",
            "message": message
        })),
    )
        .into_response()
}

/// Emit the `conversations` SSE update (the sync coordinator's post-refresh
/// event — same shape the webhook/worker paths emit).
fn emit_conversation_updated(state: &crate::http::server::AppState, conversation_local_id: i64) {
    crate::http::event_bus::notify_conversation_updated(
        &state.bus,
        &crate::events::ConversationUpdatedEvent {
            conversation_id: Some(conversation_local_id),
            conversation_number: None,
            mailbox_id: None,
            subject: None,
            reason: "sync".into(),
            at: chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
        },
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE app_state (id INTEGER PRIMARY KEY, schema_version INTEGER);
             INSERT INTO app_state (id, schema_version) VALUES (1, 28);
             CREATE TABLE conversations (
                id INTEGER PRIMARY KEY, remote_id INTEGER NOT NULL UNIQUE,
                number INTEGER, subject TEXT, preview TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                mailbox_id INTEGER NOT NULL, assignee_id INTEGER, customer_id INTEGER NOT NULL,
                priority TEXT, created_at TEXT, updated_at TEXT, closed_at TEXT,
                local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
             CREATE TABLE mailboxes (id INTEGER PRIMARY KEY, remote_id INTEGER, name TEXT);
             CREATE TABLE tags (id INTEGER PRIMARY KEY, remote_id INTEGER, name TEXT, slug TEXT, ticket_count INTEGER DEFAULT 0);
             CREATE TABLE users (id INTEGER PRIMARY KEY, remote_id INTEGER, first_name TEXT, last_name TEXT);
             CREATE TABLE conversation_threads (
                id INTEGER PRIMARY KEY AUTOINCREMENT, conversation_id INTEGER NOT NULL,
                type TEXT NOT NULL, body_text TEXT, from_type TEXT,
                created_by_user_id INTEGER, created_by_customer_id INTEGER,
                created_by_system_user_id INTEGER,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
             CREATE TABLE audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT, timestamp TEXT,
                actor TEXT, action TEXT, conversation_id INTEGER, before_state TEXT,
                after_state TEXT, remote_operation TEXT, remote_result TEXT, ai_involvement INTEGER,
                job_id INTEGER, correlation_id TEXT);",
        )
        .expect("schema");
        // jobs + outbound_jobs via the canonical paths
        crate::jobs::ensure_jobs_table(&conn).expect("jobs");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS outbound_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                conversation_id INTEGER,
                thread_id INTEGER,
                payload TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'queued',
                attempts INTEGER DEFAULT 0,
                max_attempts INTEGER DEFAULT 1,
                error TEXT,
                remote_result TEXT,
                requires_confirmation INTEGER DEFAULT 0,
                confirmed_at TEXT,
                idempotency_key TEXT UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .expect("outbound_jobs");
        conn.execute(
            "CREATE TABLE IF NOT EXISTS application_settings (
                key TEXT PRIMARY KEY, value TEXT, updated_at TEXT)",
            [],
        )
        .expect("application_settings");
        apply_m030(&conn).expect("m030");
        conn
    }

    fn seed(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, mailbox_id, customer_id)
             VALUES (9001, 101, 1, 1)",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn m030_is_idempotent() {
        let conn = fresh_db();
        apply_m030(&conn).expect("second apply");
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(conversations)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(cols.iter().any(|c| c == "snoozed_until"));
        assert_eq!(cols.iter().filter(|c| *c == "snoozed_until").count(), 1);
    }

    #[test]
    fn move_updates_mailbox_and_audits() {
        let conn = fresh_db();
        let id = seed(&conn);
        conn.execute(
            "INSERT INTO mailboxes (remote_id, name) VALUES (77, 'Billing')",
            [],
        )
        .unwrap();
        // The mailboxes row gets local id 1; the reference stores the LOCAL
        // mailbox id (updateLocalMailbox(conversationId, mailbox.id)).
        let billing_local_id = conn.last_insert_rowid();
        let resp = op_move_to_inbox(&conn, id, 77);
        assert_eq!(resp.status(), StatusCode::OK);
        let mailbox: i64 = conn
            .query_row(
                "SELECT mailbox_id FROM conversations WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mailbox, billing_local_id);
        let action: String = conn
            .query_row(
                "SELECT action FROM audit_log ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(action, "moved_inbox");
    }

    #[test]
    fn move_rejects_unknown_conversation() {
        let conn = fresh_db();
        let resp = op_move_to_inbox(&conn, 4242, 77);
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn tags_merge_semantics() {
        let conn = fresh_db();
        let id = seed(&conn);
        // add a, b
        op_update_tags(&conn, id, &json!({ "add": ["a", "b"] }));
        let tags = read_conversation_tags(&conn, id);
        assert_eq!(tags, vec!["a".to_string(), "b".to_string()]);
        // add c, remove A (case-insensitive)
        op_update_tags(&conn, id, &json!({ "add": ["c"], "remove": ["A"] }));
        let tags = read_conversation_tags(&conn, id);
        assert_eq!(tags, vec!["b".to_string(), "c".to_string()]);
        // set replaces entirely + dedupes + trims
        op_update_tags(&conn, id, &json!({ "set": [" x ", "x", "y"] }));
        let tags = read_conversation_tags(&conn, id);
        assert_eq!(tags, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn snooze_roundtrip() {
        let conn = fresh_db();
        let id = seed(&conn);
        let resp = op_snooze(&conn, id, "2026-10-05T12:00:00Z");
        assert_eq!(resp.status(), StatusCode::OK);
        let until: Option<String> = conn
            .query_row(
                "SELECT snoozed_until FROM conversations WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(until.as_deref(), Some("2026-10-05T12:00:00Z"));
        let resp = op_unsnooze(&conn, id);
        assert_eq!(resp.status(), StatusCode::OK);
        let until: Option<String> = conn
            .query_row(
                "SELECT snoozed_until FROM conversations WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(until.is_none());
    }

    #[test]
    fn eval_mode_blocks_writes() {
        let conn = fresh_db();
        let id = seed(&conn);
        let _ = conn.execute(
            "INSERT INTO application_settings (key, value, updated_at)
             VALUES ('ai_evaluation_mode', '1', datetime('now'))",
            [],
        );
        let resp = op_snooze(&conn, id, "2026-10-05T12:00:00Z");
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let until: Option<String> = conn
            .query_row(
                "SELECT snoozed_until FROM conversations WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(until.is_none());
    }

    #[test]
    fn schedule_requires_thread_in_same_conversation() {
        let conn = fresh_db();
        let id = seed(&conn);
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, from_type)
             VALUES (?1, 'reply', 'user')",
            params![id],
        )
        .unwrap();
        let thread = conn.last_insert_rowid();
        // other conversation's thread
        conn.execute(
            "INSERT INTO conversations (remote_id, number, mailbox_id, customer_id)
             VALUES (9002, 102, 1, 1)",
            [],
        )
        .unwrap();
        let other = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, from_type)
             VALUES (?1, 'reply', 'user')",
            params![other],
        )
        .unwrap();
        let other_thread = conn.last_insert_rowid();
        let resp = op_schedule_reply(&conn, id, other_thread, "2026-10-06T09:00:00Z");
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let resp = op_schedule_reply(&conn, id, thread, "2026-10-06T09:00:00Z");
        assert_eq!(resp.status(), StatusCode::OK);
        let (state, sched): (String, Option<String>) = conn
            .query_row(
                "SELECT state, scheduled_for FROM conversation_threads WHERE id = ?1",
                params![thread],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "scheduled");
        assert_eq!(sched.as_deref(), Some("2026-10-06T09:00:00Z"));
        // publish → published + null schedule
        let resp = op_publish_schedule(&conn, id, thread);
        assert_eq!(resp.status(), StatusCode::OK);
        let (state, sched): (String, Option<String>) = conn
            .query_row(
                "SELECT state, scheduled_for FROM conversation_threads WHERE id = ?1",
                params![thread],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "published");
        assert!(sched.is_none());
    }

    #[test]
    fn bulk_queues_one_job_per_conversation() {
        let conn = fresh_db();
        let resp = op_bulk_action(&conn, &[1, 2, 3], "close", &json!({}));
        assert_eq!(resp.status(), StatusCode::OK);
        let (count, kind, queue, prio): (i64, String, String, i64) = conn
            .query_row(
                "SELECT COUNT(*), MIN(type), MIN(queue), MIN(priority) FROM jobs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(count, 3);
        assert_eq!(kind, "bulk_close");
        assert_eq!(queue, "api");
        assert_eq!(prio, 1);
    }

    #[test]
    fn js_number_matches_js_semantics() {
        assert_eq!(js_number("12"), Some(12.0));
        assert_eq!(js_number(" 12 "), Some(12.0));
        assert_eq!(js_number(""), Some(0.0));
        assert_eq!(js_number("12.5"), Some(12.5));
        assert_eq!(js_number("abc"), None);
    }

    #[test]
    fn zod_int_envelope() {
        let body = json!({ "mailboxId": "x" });
        let err = zod_int_msg(&body, "mailboxId").unwrap_err();
        assert_eq!(err, "Expected number, received string");
        let body = json!({});
        let err = zod_int_msg(&body, "mailboxId").unwrap_err();
        assert_eq!(err, "Required");
        assert_eq!(
            zod_int_msg(&json!({ "mailboxId": 5 }), "mailboxId").unwrap(),
            5
        );
        let resp = zod_422("mailboxId", "Required");
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
