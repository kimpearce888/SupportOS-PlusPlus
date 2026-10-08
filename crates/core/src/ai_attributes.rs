//! General AI Attribute Layer — reference parity (v1.9.0 / M3, plan Phase 16).
//!
//! Mirrors the reference `attributeRepo.ts` + `ai/attributes.ts`:
//! - `ai_attributes` is VERSIONED via `superseded_at` (full history preserved);
//!   current = `superseded_at IS NULL`; a missing row IS the honest 'unknown'.
//! - `save_snapshot` is AUTHORITATIVE: it retires every current row first, so
//!   keys that produced no record this round honestly read 'unknown' again.
//!   Records outside the closed catalog, with a disagreeing value_type, with
//!   an enum value outside the closed vocabulary, with a non-finite number or
//!   a non-'true'/'false' boolean are DROPPED (never stored).
//! - Two layers, one snapshot: deterministic (zero AI, always available) +
//!   AI extraction (LM Studio only, cached by input hash + prompt version).
//!   Deterministic keys are authoritative for their catalog slots; AI fills
//!   only the AI-designated slots (intent/product/feature/issue/
//!   customer_goal/response_style).
//! - Attributes are local-only and never written to Help Scout.
//!
//! M033 rebuilds the old M010 `ai_attributes` shape
//! (attribute_key/evidence_excerpt/thread_ref/confidence REAL) into the
//! reference shape, migrating rows forward, then drops the old table.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ai_provider::{ChatMessage, LocalAiProvider};
use crate::catalog::{AiAttributeKey, AttributeValueType, AI_ATTRIBUTE_SCHEMA_VERSION};
use crate::error::{Error, Result};

/// The M033 migration: rebuilds `ai_attributes` to the reference shape
/// (reference migration `013_m3_copilot_attributes.ts`).
pub const M033_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS ai_attributes (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        attribute       TEXT NOT NULL,
        value           TEXT NOT NULL,
        value_type      TEXT NOT NULL,
        confidence      TEXT NOT NULL,
        source          TEXT NOT NULL,
        evidence        TEXT,
        run_id          INTEGER REFERENCES ai_runs(id) ON DELETE SET NULL,
        schema_version  TEXT NOT NULL,
        computed_at     TEXT NOT NULL DEFAULT (datetime('now')),
        superseded_at   TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_ai_attributes_current
        ON ai_attributes (conversation_id, attribute, superseded_at);
    CREATE INDEX IF NOT EXISTS idx_ai_attributes_value
        ON ai_attributes (attribute, value, superseded_at);
    CREATE INDEX IF NOT EXISTS idx_ai_attributes_run
        ON ai_attributes (run_id);
"#;

/// Whether `table` exists.
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |r| r.get(0),
    )?;
    Ok(n != 0)
}

/// Whether `table` has a column named `column`.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|c| c.ok())
        .collect();
    Ok(cols.iter().any(|c| c == column))
}

/// One legacy M010 row pulled for the M033 forward migration.
type LegacyM010Row = (i64, i64, String, String, String, String, f64, String);

/// Apply the M033 migration. Idempotent + forward-only:
/// - fresh DB → create the reference-shape table;
/// - already migrated → ensure indexes (no-op);
/// - old M010 shape (attribute_key column) → migrate rows forward inside one
///   transaction, then drop the old table.
///
/// # Errors
///
/// Returns `Error::Sqlite` if any statement fails.
pub fn apply_m033(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ai_attributes")? {
        // Fresh DB — straight to the reference shape. The app_state version
        // bump is best-effort: minimal/partial DBs (e.g. test schemas) may
        // not carry app_state, and the DDL itself must not depend on it.
        conn.execute_batch(M033_SQL)?;
        let _ = conn.execute("UPDATE app_state SET schema_version = 33 WHERE id = 1", []);
        return Ok(());
    }
    if column_exists(conn, "ai_attributes", "attribute")? {
        // Already the reference shape — idempotent re-run.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_ai_attributes_current
                ON ai_attributes (conversation_id, attribute, superseded_at);
             CREATE INDEX IF NOT EXISTS idx_ai_attributes_value
                ON ai_attributes (attribute, value, superseded_at);
             CREATE INDEX IF NOT EXISTS idx_ai_attributes_run
                ON ai_attributes (run_id);",
        )?;
        return Ok(());
    }

    // Old M010 shape (attribute_key) — migrate rows forward, then drop it.
    //
    // DB-01: two execution modes. Standalone (autocommit): this function
    // owns its transaction and toggles foreign_keys off around the table
    // rebuild (the SQLite-recommended pattern: legacy rows may predate FK
    // enforcement), restoring the prior setting afterwards. Inside a
    // caller's transaction (the versioned boot step): the caller owns
    // atomicity, and foreign_keys cannot be toggled mid-transaction, so
    // defer_foreign_keys — which IS allowed inside transactions — guards
    // the legacy rows instead.
    if conn.is_autocommit() {
        let fk_was_on: bool = conn
            .query_row("PRAGMA foreign_keys", [], |r| {
                r.get::<_, i64>(0).map(|v| v != 0)
            })
            .unwrap_or(false);
        let _ = conn.execute_batch("PRAGMA foreign_keys = OFF");
        conn.execute_batch("BEGIN")?;
        match rebuild_m010_to_reference(conn) {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                if fk_was_on {
                    let _ = conn.execute_batch("PRAGMA foreign_keys = ON");
                }
                return Err(e);
            }
        }
        if fk_was_on {
            let _ = conn.execute_batch("PRAGMA foreign_keys = ON");
        }
    } else {
        let _ = conn.execute_batch("PRAGMA defer_foreign_keys = ON");
        rebuild_m010_to_reference(conn)?;
    }
    let _ = conn.execute("UPDATE app_state SET schema_version = 33 WHERE id = 1", []);
    Ok(())
}

/// The M010 → reference-shape rebuild: rename the legacy table, create the
/// reference shape, pull every migratable legacy row forward, drop the
/// legacy table. Runs inside a transaction owned by the caller (or by
/// [`apply_m033`] when standalone).
fn rebuild_m010_to_reference(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE ai_attributes RENAME TO ai_attributes_m010;
             CREATE TABLE ai_attributes (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                attribute       TEXT NOT NULL,
                value           TEXT NOT NULL,
                value_type      TEXT NOT NULL,
                confidence      TEXT NOT NULL,
                source          TEXT NOT NULL,
                evidence        TEXT,
                run_id          INTEGER REFERENCES ai_runs(id) ON DELETE SET NULL,
                schema_version  TEXT NOT NULL,
                computed_at     TEXT NOT NULL DEFAULT (datetime('now')),
                superseded_at   TEXT
            );
             CREATE INDEX IF NOT EXISTS idx_ai_attributes_current
                ON ai_attributes (conversation_id, attribute, superseded_at);
             CREATE INDEX IF NOT EXISTS idx_ai_attributes_value
                ON ai_attributes (attribute, value, superseded_at);
             CREATE INDEX IF NOT EXISTS idx_ai_attributes_run
                ON ai_attributes (run_id);",
    )?;

    // Pull the legacy rows.
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, attribute_key, value, evidence_excerpt, thread_ref,
                confidence, created_at
         FROM ai_attributes_m010",
    )?;
    let legacy: Vec<LegacyM010Row> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .filter_map(|row| row.ok())
        .collect();
    drop(stmt);

    for (id, conversation_id, key, value, excerpt, thread_ref, confidence, created_at) in legacy {
        // Closed catalog: unknown keys have no value_type and cannot migrate.
        let Some(def) = AiAttributeKey::parse(&key) else {
            continue;
        };
        let confidence = if confidence <= 0.0 {
            "unknown"
        } else {
            "medium"
        };
        // Legacy evidence: excerpt + thread_ref, JSON-array shaped.
        let evidence = json!([{ "excerpt": excerpt, "thread_ref": thread_ref }]).to_string();
        conn.execute(
            "INSERT INTO ai_attributes
                    (id, conversation_id, attribute, value, value_type, confidence, source,
                     evidence, run_id, schema_version, computed_at, superseded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ai', ?7, NULL, ?8, ?9, NULL)",
            params![
                id,
                conversation_id,
                def.as_str(),
                value,
                def.value_type().as_str(),
                confidence,
                evidence,
                AI_ATTRIBUTE_SCHEMA_VERSION,
                created_at,
            ],
        )?;
    }

    conn.execute_batch("DROP TABLE ai_attributes_m010")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Records + rows
// ---------------------------------------------------------------------------

/// A record to store in one snapshot write (reference `AttributeRecord`).
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeRecord {
    /// The catalog key.
    pub key: AiAttributeKey,
    /// The stored value ('true'/'false' for booleans, digits for numbers).
    pub value: String,
    /// 'high' | 'medium' | 'low' | 'unknown'.
    pub confidence: &'static str,
    /// 'deterministic' | 'ai'.
    pub source: &'static str,
    /// Evidence array (excerpt + thread reference), JSON-serialized to 4000 chars.
    pub evidence: Vec<Value>,
    /// The ai_runs row this record came from (AI layer only).
    pub run_id: Option<i64>,
}

/// A stored attribute row, mapped exactly like the reference `mapRow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributeRow {
    pub id: i64,
    pub conversation_id: i64,
    pub conversation_number: Option<i64>,
    pub attribute: String,
    pub value: String,
    pub value_type: String,
    pub confidence: String,
    pub source: String,
    pub evidence: Value,
    pub run_id: Option<i64>,
    pub schema_version: String,
    pub computed_at: String,
    /// The retirement stamp for versioned rows. Kept for internal assertions
    /// (history ordering/versioning tests) but NEVER serialized: the
    /// reference `mapRow` selects `a.superseded_at` yet drops it from the
    /// mapped row, so the wire shape has no such field.
    #[serde(skip_serializing)]
    pub superseded_at: Option<String>,
}

/// The current snapshot for one conversation (reference
/// `ConversationAttributeSnapshot`): known rows + honest unknown keys.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttributeSnapshot {
    pub conversation_id: i64,
    pub conversation_number: Option<i64>,
    pub attributes: Vec<Value>,
    pub unknown: Vec<&'static str>,
    pub computed_at: Option<String>,
}

/// A conversation match from `conversations_matching` (reference `mapMatch`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributeMatch {
    pub conversation_id: i64,
    pub number: i64,
    pub subject: Option<String>,
    pub value: String,
    pub confidence: String,
    pub source: String,
    pub computed_at: String,
}

/// One distribution entry (reference `AiAttributeDistribution`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttributeDistribution {
    pub attribute: &'static str,
    pub label: &'static str,
    pub value_type: &'static str,
    pub total_conversations: i64,
    pub known: i64,
    pub unknown: i64,
    pub values: Vec<AttributeValueCount>,
}

/// One `{value, count}` pair inside a distribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeValueCount {
    pub value: String,
    pub count: i64,
}

/// The port's soft-delete marker for conversations (the reference uses
/// `deleted_at IS NULL`; the port marks deleted rows with `status='deleted'`).
/// Interpolated WITHOUT a table prefix: in every query below, `status` is
/// unambiguous (only `conversations` carries it) and the parentheses keep
/// the OR-scoping correct inside larger WHERE clauses.
const NOT_DELETED: &str = "(status IS NULL OR status != 'deleted')";

// ---------------------------------------------------------------------------
// Repository (reference attributeRepo.ts)
// ---------------------------------------------------------------------------

/// Validate + insert ONE record inside an open transaction (shared by
/// `save_snapshot` and `upsert_single`). Records with an enum value outside
/// the closed vocabulary, a non-finite number or a non-'true'/'false'
/// boolean are DROPPED (never stored) — the value_type always comes from
/// the catalog, so a disagreeing value_type cannot exist by construction.
/// Returns whether a row was actually inserted.
fn insert_record(
    tx: &rusqlite::Transaction<'_>,
    conversation_id: i64,
    r: &AttributeRecord,
    run_id: Option<i64>,
) -> Result<bool> {
    let def = r.key;
    if def.value_type() == AttributeValueType::Enum && !def.values().contains(&r.value.as_str()) {
        return Ok(false); // closed vocabulary
    }
    if def.value_type() == AttributeValueType::Number
        && parse_js_number(&r.value).is_none_or(|f| !f.is_finite())
    {
        return Ok(false); // non-finite numbers are never stored
    }
    if def.value_type() == AttributeValueType::Boolean && r.value != "true" && r.value != "false" {
        return Ok(false);
    }
    let evidence = {
        let s = serde_json::to_string(&r.evidence)
            .map_err(|e| Error::Config(format!("evidence serialization failed: {e}")))?;
        s.chars().take(4000).collect::<String>()
    };
    tx.execute(
        "INSERT INTO ai_attributes
            (conversation_id, attribute, value, value_type, confidence, source, evidence,
             run_id, schema_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            conversation_id,
            def.as_str(),
            r.value,
            def.value_type().as_str(),
            r.confidence,
            r.source,
            evidence,
            r.run_id.or(run_id),
            AI_ATTRIBUTE_SCHEMA_VERSION,
        ],
    )?;
    Ok(true)
}

/// Write a full attribute snapshot for one conversation (one transaction).
/// The snapshot is AUTHORITATIVE: retire every current row first so keys that
/// produced no record this round honestly read 'unknown' again. History is
/// preserved (versioning, plan Phase 16).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the transaction fails.
pub fn save_snapshot(
    conn: &mut Connection,
    conversation_id: i64,
    records: &[AttributeRecord],
    run_id: Option<i64>,
) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE ai_attributes SET superseded_at = datetime('now')
         WHERE conversation_id = ?1 AND superseded_at IS NULL",
        params![conversation_id],
    )?;
    for r in records {
        insert_record(&tx, conversation_id, r, run_id)?;
    }
    tx.commit()?;
    Ok(())
}

/// Upsert ONE key's current row (one transaction). A port-side adapter for
/// the legacy M010-era per-key setter (`ai_analysis::set_attribute`): the
/// reference itself only ever writes full snapshots via `save_snapshot`, so
/// this retires just the given key's current rows — sibling keys stay
/// untouched — then inserts through the same validation rules. Returns the
/// inserted row id (0 when the record is dropped by validation).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the transaction fails.
pub fn upsert_single(
    conn: &mut Connection,
    conversation_id: i64,
    record: AttributeRecord,
) -> Result<i64> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE ai_attributes SET superseded_at = datetime('now')
         WHERE conversation_id = ?1 AND attribute = ?2 AND superseded_at IS NULL",
        params![conversation_id, record.key.as_str()],
    )?;
    let inserted = insert_record(&tx, conversation_id, &record, None)?;
    let id = if inserted { tx.last_insert_rowid() } else { 0 };
    tx.commit()?;
    Ok(id)
}

/// Current rows for one conversation (empty array = all unknown).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn current_for_conversation(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Vec<AttributeRow>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.conversation_id, a.attribute, a.value, a.value_type, a.confidence,
                a.source, a.evidence, a.run_id, a.schema_version, a.computed_at,
                c.number AS conversation_number
         FROM ai_attributes a JOIN conversations c ON c.id = a.conversation_id
         WHERE a.conversation_id = ?1 AND a.superseded_at IS NULL",
    )?;
    let rows = stmt
        .query_map(params![conversation_id], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Full version history for one attribute (newest first).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn history(
    conn: &Connection,
    conversation_id: i64,
    attribute: AiAttributeKey,
    limit: i64,
) -> Result<Vec<AttributeRow>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.conversation_id, a.attribute, a.value, a.value_type, a.confidence,
                a.source, a.evidence, a.run_id, a.schema_version, a.computed_at,
                c.number AS conversation_number, a.superseded_at
         FROM ai_attributes a JOIN conversations c ON c.id = a.conversation_id
         WHERE a.conversation_id = ?1 AND a.attribute = ?2
         ORDER BY a.id DESC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(
            params![conversation_id, attribute.as_str(), limit],
            map_history_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The comparison operators allowed by `conversations_matching`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOp {
    Equals,
    NotEquals,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
    Unknown,
}

impl MatchOp {
    /// The operators allowed for a value type (reference allowlist).
    #[must_use]
    pub fn allowed(value_type: AttributeValueType) -> &'static [Self] {
        match value_type {
            AttributeValueType::Number => &[
                Self::Equals,
                Self::NotEquals,
                Self::Gt,
                Self::Gte,
                Self::Lt,
                Self::Lte,
                Self::Unknown,
            ],
            _ => &[Self::Equals, Self::NotEquals, Self::Contains, Self::Unknown],
        }
    }

    /// Parse + allowlist: an invalid operator falls back to 'equals'
    /// (reference route behavior).
    #[must_use]
    pub fn parse(raw: &str, value_type: AttributeValueType) -> Self {
        let op = match raw {
            "equals" => Self::Equals,
            "not_equals" => Self::NotEquals,
            "contains" => Self::Contains,
            "gt" => Self::Gt,
            "gte" => Self::Gte,
            "lt" => Self::Lt,
            "lte" => Self::Lte,
            "unknown" => Self::Unknown,
            _ => return Self::Equals,
        };
        if Self::allowed(value_type).contains(&op) {
            op
        } else {
            Self::Equals
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Equals => "equals",
            Self::NotEquals => "not_equals",
            Self::Contains => "contains",
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Unknown => "unknown",
        }
    }
}

/// Escape LIKE wildcards with backslashes (reference: value.replace(/[\\%_]/g)).
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// JavaScript `Number(value)` semantics: trimmed parse, '' → 0, invalid → NaN.
fn parse_js_number(raw: &str) -> Option<f64> {
    let t = raw.trim();
    if t.is_empty() {
        return Some(0.0);
    }
    t.parse::<f64>().ok()
}

/// Conversations currently matching an attribute test (searchable /
/// drillable). `attribute` must be in the closed catalog (checked by the
/// caller); `limit` is clamped 1..=200 here as in the reference repo.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn conversations_matching(
    conn: &Connection,
    attribute: AiAttributeKey,
    op: MatchOp,
    value: &str,
    limit: i64,
) -> Result<Vec<AttributeMatch>> {
    let cap = limit.clamp(1, 200);
    let map_match = |r: &rusqlite::Row<'_>| -> rusqlite::Result<AttributeMatch> {
        Ok(AttributeMatch {
            conversation_id: r.get(0)?,
            number: r.get(1)?,
            subject: r.get(2)?,
            value: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            confidence: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            source: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            computed_at: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        })
    };

    if op == MatchOp::Unknown {
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.number, c.subject, '' AS value, 'unknown' AS confidence,
                    'none' AS source, '' AS computed_at
             FROM conversations c
             WHERE {NOT_DELETED} AND NOT EXISTS (
               SELECT 1 FROM ai_attributes a
               WHERE a.conversation_id = c.id AND a.attribute = ?1 AND a.superseded_at IS NULL
             )
             ORDER BY c.number LIMIT ?2"
        ))?;
        let rows = stmt
            .query_map(params![attribute.as_str(), cap], map_match)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        return Ok(rows);
    }

    let is_number = attribute.value_type() == AttributeValueType::Number;
    // Number ops compare CAST(a.value AS REAL); a non-finite target matches
    // nothing (reference: `if (def.value_type === 'number' && !Number.isFinite(target)) return []`).
    let (table, cmp, param): (&str, &str, String) = if is_number {
        let Some(target) = parse_js_number(value) else {
            return Ok(Vec::new());
        };
        if !target.is_finite() {
            return Ok(Vec::new());
        }
        let cmp = match op {
            MatchOp::Equals => "= ?",
            MatchOp::NotEquals => "!= ?",
            MatchOp::Gt => "> ?",
            MatchOp::Gte => ">= ?",
            MatchOp::Lt => "< ?",
            MatchOp::Lte => "<= ?",
            _ => return Ok(Vec::new()),
        };
        ("CAST(a.value AS REAL)", cmp, format!("{target}"))
    } else {
        match op {
            MatchOp::Contains => (
                "LOWER(a.value)",
                "LIKE ? ESCAPE '\\'",
                format!("%{}%", escape_like(value)),
            ),
            MatchOp::Equals => ("LOWER(a.value)", "= ?", value.to_lowercase()),
            MatchOp::NotEquals => ("LOWER(a.value)", "!= ?", value.to_lowercase()),
            _ => return Ok(Vec::new()),
        }
    };

    let sql = format!(
        "SELECT a.conversation_id, c.number, c.subject, a.value, a.confidence, a.source,
                a.computed_at
         FROM ai_attributes a JOIN conversations c ON c.id = a.conversation_id
         WHERE a.attribute = ? AND a.superseded_at IS NULL AND {NOT_DELETED} AND {table} {cmp}
         ORDER BY c.number LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![attribute.as_str(), param, cap], map_match)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Distribution + honest coverage per attribute over non-deleted conversations.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn distributions(conn: &Connection) -> Result<Vec<AttributeDistribution>> {
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM conversations WHERE {NOT_DELETED}"),
        [],
        |r| r.get(0),
    )?;
    let mut out = Vec::with_capacity(AiAttributeKey::ALL.len());
    for def in AiAttributeKey::ALL {
        let mut stmt = conn.prepare(
            "SELECT value, COUNT(DISTINCT conversation_id) AS count FROM ai_attributes
             WHERE attribute = ?1 AND superseded_at IS NULL
             GROUP BY value ORDER BY count DESC, value",
        )?;
        let rows: Vec<(String, i64)> = stmt
            .query_map(params![def.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|row| row.ok())
            .collect();
        let known: i64 = rows.iter().map(|(_, c)| c).sum();
        out.push(AttributeDistribution {
            attribute: def.as_str(),
            label: def.label(),
            value_type: def.value_type().as_str(),
            total_conversations: total,
            known,
            unknown: (total - known).max(0),
            values: rows
                .into_iter()
                .take(25)
                .map(|(value, count)| AttributeValueCount { value, count })
                .collect(),
        });
    }
    Ok(out)
}

/// Distinct current values for one attribute (autocomplete for filters).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn distinct_values(
    conn: &Connection,
    attribute: AiAttributeKey,
    limit: i64,
) -> Result<Vec<String>> {
    let cap = limit.clamp(1, 200);
    let mut stmt = conn.prepare(
        "SELECT DISTINCT value FROM ai_attributes
         WHERE attribute = ?1 AND superseded_at IS NULL
         ORDER BY value LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![attribute.as_str(), cap], |r| r.get::<_, String>(0))?
        .filter_map(|row| row.ok())
        .collect();
    Ok(rows)
}

/// Conversations with at least one current attribute row (backfill targeting).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn conversations_with_attributes(conn: &Connection, limit: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT conversation_id FROM ai_attributes
         WHERE superseded_at IS NULL LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| r.get::<_, i64>(0))?
        .filter_map(|row| row.ok())
        .collect();
    Ok(rows)
}

/// Map one row exactly like the reference `mapRow` (evidence parsed as a JSON
/// array, defaulting to `[]`; numbers coerced; nulls preserved).
fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AttributeRow> {
    let evidence: Option<String> = r.get(7)?;
    let evidence = evidence
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| json!([]));
    Ok(AttributeRow {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        conversation_number: r.get::<_, Option<i64>>(11)?,
        attribute: r.get(2)?,
        value: r.get(3)?,
        value_type: r.get(4)?,
        confidence: r.get(5)?,
        source: r.get(6)?,
        evidence,
        run_id: r.get(8)?,
        schema_version: r.get(9)?,
        computed_at: r.get(10)?,
        superseded_at: None,
    })
}

/// Map one history row (same as `map_row` + the superseded_at stamp).
fn map_history_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AttributeRow> {
    let mut row = map_row(r)?;
    row.superseded_at = r.get(12)?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Service (reference ai/attributes.ts)
// ---------------------------------------------------------------------------

/// Current snapshot with honest unknown keys listed separately. `None` = the
/// conversation does not exist (or is deleted).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn snapshot(conn: &Connection, conversation_id: i64) -> Result<Option<AttributeSnapshot>> {
    let conv: Option<(i64, Option<i64>)> = conn
        .query_row(
            &format!("SELECT id, number FROM conversations WHERE id = ?1 AND {NOT_DELETED}"),
            params![conversation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((id, number)) = conv else {
        return Ok(None);
    };
    let rows = current_for_conversation(conn, id)?;
    let known: Vec<&str> = rows.iter().map(|r| r.attribute.as_str()).collect();
    let unknown: Vec<&'static str> = AiAttributeKey::ALL
        .iter()
        .map(|k| k.as_str())
        .filter(|k| !known.contains(k))
        .collect();
    let computed_at = rows
        .iter()
        .map(|r| r.computed_at.as_str())
        .max()
        .map(str::to_string);
    let attributes = rows
        .into_iter()
        .map(|r| {
            let mut v = serde_json::to_value(&r).unwrap_or_else(|_| json!({}));
            if let Value::Object(map) = &mut v {
                map.insert("status".into(), json!("known"));
            }
            v
        })
        .collect();
    Ok(Some(AttributeSnapshot {
        conversation_id: id,
        conversation_number: number,
        attributes,
        unknown,
        computed_at,
    }))
}

/// The prompt version for the AI extraction layer (reference
/// `PROMPT_VERSIONS.ATTRIBUTE_EXTRACTION`).
pub const ATTRIBUTE_EXTRACTION_PROMPT_VERSION: &str = "attribute_extraction_v1";

/// The extraction system prompt (reference `ATTRIBUTE_EXTRACTION_SYSTEM`).
const ATTRIBUTE_EXTRACTION_SYSTEM: &str = "\
You extract structured attributes from a customer support conversation. You are part of a LOCAL-first support tool: everything you output is stored as versioned, evidence-backed local metadata - never sent to the customer.\n\
Rules:\n\
- Extract ONLY what the messages support. A missing attribute is simply omitted - never invent values.\n\
- \"intent\" MUST be exactly one of: question | bug_report | feature_request | billing | how_to | account_management | feedback | other.\n\
- \"response_style\" MUST be exactly one of: concise | detailed | step_by_step | technical | conversational | outcome_focused - pick the style the CUSTOMER's own messages ask for, not what you would write.\n\
- \"product\", \"feature\", \"issue\", \"customer_goal\" are short free text (max ~12 words each).\n\
- For every attribute with confidence \"medium\" or \"high\" you MUST include an evidence_excerpt copied from the messages provided, plus the thread id it came from.\n\
- Use confidence \"low\" when the signal is weak or ambiguous.\n\
- Respond with a single JSON object, no prose.\n\
\n\
JSON shape:\n\
{\"attributes\": [{\"attribute\": \"intent|product|feature|issue|customer_goal|response_style\", \"value\": \"...\", \"confidence\": \"high|medium|low|unknown\", \"evidence_excerpt\": \"...\", \"evidence_thread_local_id\": 123}]}";

/// One customer message for analysis (thread-local id + text).
#[derive(Debug, Clone, PartialEq)]
pub struct CustomerMessage {
    pub thread_local_id: i64,
    pub text: String,
}

/// Gather the customer's own messages (reference `customerMessages`): type
/// 'customer', ordered oldest first, capped at 30, text capped at 1200 chars,
/// empty bodies dropped.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn customer_messages(conn: &Connection, conversation_id: i64) -> Result<Vec<CustomerMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(body, '') FROM conversation_threads
         WHERE conversation_id = ?1 AND thread_type = 'customer'
         ORDER BY created_at ASC, id ASC LIMIT 30",
    )?;
    let rows: Vec<CustomerMessage> = stmt
        .query_map(params![conversation_id], |r| {
            Ok(CustomerMessage {
                thread_local_id: r.get(0)?,
                text: r.get(1)?,
            })
        })?
        .filter_map(|row| row.ok())
        .map(|m| {
            let text: String = m.text.chars().take(1200).collect();
            CustomerMessage {
                thread_local_id: m.thread_local_id,
                text,
            }
        })
        .filter(|m| !m.text.trim().is_empty())
        .collect();
    Ok(rows)
}

/// Everything the computation needs after the (optional, await-able) AI call:
/// the deterministic records, the extraction prompt + input hash, and any
/// cached extraction. Splitting prepare → AI call → finish lets HTTP handlers
/// release the connection mutex while the provider call is in flight.
#[derive(Debug, Clone)]
pub struct PreparedComputation {
    /// The conversation being computed.
    pub conversation_id: i64,
    /// Layer-1 deterministic records (zero AI, always present).
    pub records: Vec<AttributeRecord>,
    /// The extraction user prompt (`None` when there are no customer messages
    /// or no model is configured — the AI layer is skipped entirely).
    pub prompt: Option<String>,
    /// The cache key for the extraction (reference input-hash scheme).
    pub input_hash: String,
    /// A cached extraction hit: (run_id, parsed output).
    pub cached: Option<(i64, Value)>,
}

/// Phase 1 of `compute`: validate the conversation, run the deterministic
/// layer, gather the messages/prompt, and check the extraction cache.
/// Returns `None` = conversation not found (or deleted).
///
/// # Errors
///
/// Returns `Error::Sqlite` if a query fails.
pub fn prepare_computation(
    conn: &Connection,
    conversation_id: i64,
    model: &str,
    force: bool,
) -> Result<Option<PreparedComputation>> {
    let exists: Option<i64> = conn
        .query_row(
            &format!("SELECT id FROM conversations WHERE id = ?1 AND {NOT_DELETED}"),
            params![conversation_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    if exists.is_none() {
        return Ok(None);
    }

    // ---- Layer 1: deterministic (zero AI) ----
    let records = deterministic_records(conn, conversation_id)?;

    // ---- Layer 2 inputs: messages + prompt + cache ----
    let messages = customer_messages(conn, conversation_id)?;
    let (prompt, input_hash) = if messages.is_empty() || model.is_empty() {
        (None, String::new())
    } else {
        let prompt = build_extraction_user(conn, conversation_id, &messages);
        let hash = crate::embeddings::content_hash(&prompt);
        (Some(prompt), hash)
    };

    let cached = if force || prompt.is_none() {
        None
    } else {
        // Only cache hits whose output actually carries an attributes array count.
        lookup_cached_extraction(conn, &input_hash, model)?
            .filter(|(_, v)| v.get("attributes").is_some_and(|a| a.as_array().is_some()))
    };

    Ok(Some(PreparedComputation {
        conversation_id,
        records,
        prompt,
        input_hash,
        cached,
    }))
}

/// Phase 2 of `compute`: the await-able provider call. Touches NO database —
/// safe to run while the connection mutex is released. Returns the raw model
/// output; `None` on unavailability or failure (honest degradation, the
/// reference `failRun` path).
pub async fn run_extraction(
    provider: &dyn LocalAiProvider,
    model: &str,
    prompt: &str,
) -> Option<String> {
    if !provider.is_available().await {
        return None;
    }
    match provider
        .chat(
            model,
            &[
                ChatMessage {
                    role: "system".into(),
                    content: ATTRIBUTE_EXTRACTION_SYSTEM.into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: prompt.to_string(),
                },
            ],
        )
        .await
    {
        Ok(response) => Some(response.content),
        Err(_) => None,
    }
}

/// Phase 3 of `compute`: merge the AI extraction into the deterministic
/// records (AI fills only its designated slots), persist the snapshot and
/// return it. `fresh_output` is the raw provider content from phase 2 (only
/// consulted when there was no cache hit). Returns `None` = conversation not
/// found.
///
/// # Errors
///
/// Returns `Error::Sqlite` for DB failures.
pub fn finish_computation(
    conn: &mut Connection,
    prepared: PreparedComputation,
    fresh_output: Option<String>,
    model: &str,
) -> Result<Option<AttributeSnapshot>> {
    let PreparedComputation {
        conversation_id,
        mut records,
        prompt,
        input_hash,
        cached,
    } = prepared;

    let mut ai_run_id: Option<i64> = None;
    let mut extracted: Option<Value> = None;

    if let Some((run_id, cached_value)) = cached {
        extracted = Some(cached_value);
        ai_run_id = Some(run_id);
    } else if let (Some(_), Some(raw)) = (&prompt, fresh_output) {
        // Only a parseable object carrying an attributes array counts; a
        // provider echoing prose degrades honestly to the deterministic half.
        if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
            if parsed
                .get("attributes")
                .is_some_and(|a| a.as_array().is_some())
            {
                let run_id = store_extraction_run(conn, &input_hash, model, &raw)?;
                extracted = Some(parsed);
                ai_run_id = Some(run_id);
            }
        }
    }

    if let Some(Value::Object(map)) = extracted {
        if let Some(Value::Array(attrs)) = map.get("attributes") {
            merge_extraction(&mut records, attrs, ai_run_id);
        }
    }

    save_snapshot(conn, conversation_id, &records, ai_run_id)?;
    snapshot(conn, conversation_id)
}

/// Merge parsed extraction attributes into the record set. AI may only fill
/// its designated slots; deterministic slots are authoritative.
fn merge_extraction(records: &mut Vec<AttributeRecord>, attrs: &[Value], ai_run_id: Option<i64>) {
    for a in attrs {
        let Some(obj) = a.as_object() else { continue };
        let Some(attr_name) = obj.get("attribute").and_then(Value::as_str) else {
            continue;
        };
        let Some(key) = AiAttributeKey::parse(attr_name) else {
            continue; // closed catalog
        };
        if !is_ai_slot(key) {
            continue; // deterministic slots are authoritative
        }
        let Some(raw_value) = obj.get("value").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = normalize_ai_value(key, raw_value) else {
            continue; // enum violation = do not store (honest)
        };
        let mut confidence = obj
            .get("confidence")
            .and_then(Value::as_str)
            .and_then(parse_confidence)
            .unwrap_or("unknown");
        let excerpt = obj.get("evidence_excerpt").and_then(Value::as_str);
        // Evidence mandate: medium/high confidence requires an excerpt.
        if (confidence == "high" || confidence == "medium") && excerpt.is_none() {
            confidence = "low";
        }
        let evidence = match excerpt {
            Some(e) => vec![json!({
                "excerpt": e.chars().take(500).collect::<String>(),
                "thread_local_id": obj
                    .get("evidence_thread_local_id")
                    .and_then(Value::as_i64),
            })],
            None => Vec::new(),
        };
        records.push(AttributeRecord {
            key,
            value,
            confidence,
            source: "ai",
            evidence,
            run_id: ai_run_id,
        });
    }
}

/// Compute + persist the attribute snapshot for one conversation.
/// Deterministic layer always runs; the AI layer runs only when the provider
/// is enabled and reachable, with run caching. Returns the fresh snapshot
/// (`None` = conversation not found).
///
/// # Errors
///
/// Returns `Error::Sqlite` for DB failures. Provider failures degrade
/// honestly to a deterministic-only snapshot (never an error), mirroring the
/// reference `compute()`.
pub async fn compute(
    conn: &mut Connection,
    provider: &dyn LocalAiProvider,
    model: &str,
    conversation_id: i64,
    force: bool,
) -> Result<Option<AttributeSnapshot>> {
    let Some(prepared) = prepare_computation(conn, conversation_id, model, force)? else {
        return Ok(None);
    };
    let fresh = match &prepared.prompt {
        Some(prompt) if prepared.cached.is_none() => run_extraction(provider, model, prompt).await,
        _ => None,
    };
    finish_computation(conn, prepared, fresh, model)
}

/// AI may only fill these catalog slots (deterministic slots are
/// authoritative). Mirrors the reference `isAiSlot`.
fn is_ai_slot(key: AiAttributeKey) -> bool {
    matches!(
        key,
        AiAttributeKey::Intent
            | AiAttributeKey::Product
            | AiAttributeKey::Feature
            | AiAttributeKey::Issue
            | AiAttributeKey::CustomerGoal
            | AiAttributeKey::ResponseStyle
    )
}

/// Parse an operational confidence word.
fn parse_confidence(s: &str) -> Option<&'static str> {
    match s {
        "high" => Some("high"),
        "medium" => Some("medium"),
        "low" => Some("low"),
        "unknown" => Some("unknown"),
        _ => None,
    }
}

/// Enum-closed normalization; `None` = do not store (honest unknown).
/// Mirrors the reference `normalizeAiValue`.
fn normalize_ai_value(key: AiAttributeKey, value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if key.value_type() == AttributeValueType::Enum && !key.values().contains(&v) {
        return None;
    }
    Some(v.chars().take(300).collect())
}

/// The extraction user prompt (reference `buildAttributeExtractionUser`).
fn build_extraction_user(
    conn: &Connection,
    conversation_id: i64,
    messages: &[CustomerMessage],
) -> String {
    let subject: String = conn
        .query_row(
            "SELECT COALESCE(subject, '') FROM conversations WHERE id = ?1",
            params![conversation_id],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let subject: String = subject.chars().take(300).collect();
    let mut parts = vec![
        format!("SUBJECT: {subject}"),
        "CUSTOMER MESSAGES (oldest first):".into(),
    ];
    for m in messages {
        parts.push(format!("  [thread {}] {}", m.thread_local_id, m.text));
    }
    parts.push(String::new());
    parts.push("Extract the attributes that are actually supported by these messages.".into());
    parts.join("\n")
}

/// Store an extraction run in the ai_runs cache; returns the run id.
fn store_extraction_run(
    conn: &Connection,
    input_hash: &str,
    model: &str,
    output: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            input_hash,
            ATTRIBUTE_EXTRACTION_PROMPT_VERSION,
            model,
            output
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Look up a cached extraction run: (run_id, parsed output).
fn lookup_cached_extraction(
    conn: &Connection,
    input_hash: &str,
    model: &str,
) -> Result<Option<(i64, Value)>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, response_json FROM ai_runs
             WHERE input_hash = ?1 AND prompt_version = ?2 AND model = ?3
             ORDER BY created_at DESC, id DESC LIMIT 1",
            params![input_hash, ATTRIBUTE_EXTRACTION_PROMPT_VERSION, model],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    Ok(row.and_then(|(id, json)| serde_json::from_str::<Value>(&json).ok().map(|v| (id, v))))
}

// ---------------------------------------------------------------------------
// Deterministic layer (zero AI) — ported from the reference heuristics
// ---------------------------------------------------------------------------

/// Marker vocabularies (reference `heuristics.ts`).
const TECHNICAL_VOCAB: &[&str] = &[
    "api",
    "endpoint",
    "webhook",
    "payload",
    "json",
    "oauth",
    "token",
    "http",
    "https",
    "ssl",
    "tls",
    "dns",
    "timezone",
    "utc",
    "cron",
    "queue",
    "cache",
    "latency",
    "http status",
    "401",
    "403",
    "404",
    "500",
    "console",
    "log",
    "stack trace",
    "exception",
    "database",
    "sql",
    "index",
    "migration",
    "deploy",
    "build",
    "header",
    "request",
    "response",
    "callback",
    "integration",
    "sdk",
    "environment variable",
    "rate limit",
];
const URGENCY_MARKERS: &[&str] = &[
    "urgent",
    "asap",
    "immediately",
    "right now",
    "today",
    "as soon as possible",
    "deadline",
    "before our",
    "blocking",
    "production is down",
    "outage",
    "critical",
];
const FRUSTRATION_MARKERS: &[&str] = &[
    "again",
    "still",
    "already",
    "third time",
    "multiple times",
    "repeatedly",
    "frustrat",
    "unacceptable",
    "ridiculous",
    "seriously",
    "not working",
    "never works",
    "every time",
    "tired of",
    "disappointed",
    "kept",
    "nobody",
    "no one",
    "still not",
];
const ESCALATION_MARKERS: &[&str] = &[
    "escalate",
    "manager",
    "supervisor",
    "complaint",
    "legal",
    "cancel our",
    "terminate",
    "switching to",
];
const ACTION_EXPECTATION_MARKERS: &[&str] = &[
    "fix",
    "resolve",
    "restore",
    "refund",
    "escalate",
    "asap",
    "immediately",
    "compensation",
];

/// A heuristic evidence pointer: excerpt + the thread it came from.
fn find_evidence(messages: &[CustomerMessage], hit: impl Fn(&str) -> bool) -> Option<Value> {
    for m in messages {
        if hit(&m.text.to_lowercase()) {
            return Some(json!({
                "excerpt": excerpt(&m.text, 220),
                "thread_local_id": m.thread_local_id,
            }));
        }
    }
    None
}

/// Collapse whitespace + cap at ~220 chars with an ellipsis (reference `excerpt`).
fn excerpt(text: &str, max_len: usize) -> String {
    let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() <= max_len {
        clean
    } else {
        let head: String = clean.chars().take(max_len - 3).collect();
        format!("{head}\u{2026}")
    }
}

/// Whether a byte is a JS word character (`\w` = [A-Za-z0-9_]).
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Word-boundary marker match (reference `hasMarker`: `\b<marker>\b`, case
/// insensitive). "against" must not match "again".
fn has_marker(haystack_lower: &str, marker: &str) -> bool {
    let bytes = haystack_lower.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = haystack_lower[start..].find(marker) {
        let abs = start + pos;
        let end = abs + marker.len();
        let before_ok = abs == 0 || !is_word_byte(bytes[abs - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Count how many markers hit with word boundaries (reference `countMarkers`).
fn count_markers(haystack_lower: &str, markers: &[&str]) -> usize {
    markers
        .iter()
        .filter(|m| has_marker(haystack_lower, m))
        .count()
}

/// Derive the deterministic signal set for one conversation from its customer
/// messages. Returns None when the conversation has no customer messages.
struct DeterministicSignals {
    urgency: &'static str,
    urgency_confidence: &'static str,
    urgency_evidence: Option<Value>,
    frustration: &'static str,
    frustration_confidence: &'static str,
    frustration_evidence: Option<Value>,
    technical: &'static str,
    technical_confidence: &'static str,
    technical_evidence: Option<Value>,
    escalation: bool,
    escalation_evidence: Option<Value>,
    question_count: usize,
    response_style: Option<&'static str>,
    response_style_evidence: Option<Value>,
    customer_goal: Option<String>,
}

fn derive_signals(messages: &[CustomerMessage]) -> Option<DeterministicSignals> {
    if messages.is_empty() {
        return None;
    }
    let all: String = messages
        .iter()
        .map(|m| m.text.to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");

    // Urgency: >=3 marker hits or outage phrasing → high; >=1 → moderate; else none.
    let urgency_hits = count_markers(&all, URGENCY_MARKERS);
    let urgency = if urgency_hits >= 3
        || ["production is down", "outage", "emergency"]
            .iter()
            .any(|p| all.contains(p))
    {
        "high"
    } else if urgency_hits >= 1 {
        "moderate"
    } else {
        "none"
    };
    let urgency_confidence = if urgency_hits >= 1 { "medium" } else { "low" };
    let urgency_evidence = find_evidence(messages, |t| {
        URGENCY_MARKERS.iter().any(|m| has_marker(t, m))
    });

    // Frustration: >=3 → strong; >=1 → moderate; else none.
    let frustration_hits = count_markers(&all, FRUSTRATION_MARKERS);
    let frustration = if frustration_hits >= 3 {
        "strong"
    } else if frustration_hits >= 1 {
        "moderate"
    } else {
        "none"
    };
    let frustration_confidence = if frustration_hits >= 1 {
        "medium"
    } else {
        "low"
    };
    let frustration_evidence = find_evidence(messages, |t| {
        FRUSTRATION_MARKERS.iter().any(|m| has_marker(t, m))
    });

    // Technical vocabulary: >=4 highly_technical; >=2 technical; ==1 mixed; else non_technical.
    let tech_hits = count_markers(&all, TECHNICAL_VOCAB);
    let technical = if tech_hits >= 4 {
        "highly_technical"
    } else if tech_hits >= 2 {
        "technical"
    } else if tech_hits == 1 {
        "mixed"
    } else {
        "non_technical"
    };
    let technical_confidence = if tech_hits >= 1 { "medium" } else { "low" };
    let technical_evidence = find_evidence(messages, |t| {
        TECHNICAL_VOCAB.iter().any(|v| has_marker(t, v))
    });

    // Escalation intent (reference expectation dimension === 'escalation').
    let escalation = ESCALATION_MARKERS.iter().any(|m| has_marker(&all, m));
    let escalation_evidence = find_evidence(messages, |t| {
        ESCALATION_MARKERS.iter().any(|m| has_marker(t, m))
    });

    // Question count: '?' occurrences across all customer messages.
    let question_count = messages.iter().map(|m| m.text.matches('?').count()).sum();

    // Response preference: heuristics only emit it from explicit phrasing
    // (reference `heuristicResponsePreference`).
    let (response_style, response_style_evidence) = if all.contains("concise")
        || has_marker(&all, "short answer")
        || has_marker(&all, "no lengthy")
        || has_marker(&all, "brief answer")
        || has_marker(&all, "be brief")
        || all.contains("keep it short")
        || all.contains("keep this short")
        || all.contains("keep it brief")
        || all.contains("keep this brief")
    {
        (
            Some("concise"),
            find_evidence(messages, |t| {
                t.contains("concise")
                    || t.contains("short answer")
                    || t.contains("no lengthy")
                    || t.contains("brief answer")
                    || t.contains("be brief")
                    || t.contains("keep it short")
                    || t.contains("keep this short")
                    || t.contains("keep it brief")
                    || t.contains("keep this brief")
            }),
        )
    } else if all.contains("step by step")
        || all.contains("step-by-step")
        || all.contains("walk me through")
        || has_marker(&all, "instructions")
    {
        (
            Some("step_by_step"),
            find_evidence(messages, |t| {
                t.contains("step by step")
                    || t.contains("step-by-step")
                    || t.contains("walk me through")
                    || t.contains("instructions")
            }),
        )
    } else if has_marker(&all, "detailed")
        || has_marker(&all, "thorough")
        || all.contains("in depth")
        || all.contains("in-depth")
        || all.contains("full explanation")
        || has_marker(&all, "comprehensive")
    {
        (
            Some("detailed"),
            find_evidence(messages, |t| {
                t.contains("detailed")
                    || t.contains("thorough")
                    || t.contains("in depth")
                    || t.contains("in-depth")
                    || t.contains("full explanation")
                    || t.contains("comprehensive")
            }),
        )
    } else {
        (None, None)
    };

    // Customer goal: the first customer message, when it reads like a request
    // (the reference reads it from the interaction card's customer_goal; the
    // port derives it from the opening message).
    let customer_goal = messages
        .first()
        .map(|m| excerpt(&m.text, 300))
        .filter(|g| !g.trim().is_empty());

    Some(DeterministicSignals {
        urgency,
        urgency_confidence,
        urgency_evidence,
        frustration,
        frustration_confidence,
        frustration_evidence,
        technical,
        technical_confidence,
        technical_evidence,
        escalation,
        escalation_evidence,
        question_count,
        response_style,
        response_style_evidence,
        customer_goal,
    })
}

/// Deterministic attributes from stored local facts (zero AI). Mirrors the
/// reference `deterministicRecords`: observable urgency / frustration /
/// technical language / escalation intent / question count / response style /
/// risk composite from the customer's messages, plus known-issue and
/// issue-cluster membership from the local tables.
///
/// # Errors
///
/// Returns `Error::Sqlite` if a query fails.
pub fn deterministic_records(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Vec<AttributeRecord>> {
    let mut out: Vec<AttributeRecord> = Vec::new();
    let messages = customer_messages(conn, conversation_id)?;
    let signals = derive_signals(&messages);

    if let Some(s) = &signals {
        let mk = |key: AiAttributeKey,
                  value: String,
                  confidence: &'static str,
                  evidence: Vec<Value>| AttributeRecord {
            key,
            value,
            confidence,
            source: "deterministic",
            evidence,
            run_id: None,
        };
        let evidence_vec =
            |e: &Option<Value>| e.as_ref().map(|v| vec![v.clone()]).unwrap_or_default();

        out.push(mk(
            AiAttributeKey::Urgency,
            s.urgency.to_string(),
            s.urgency_confidence,
            evidence_vec(&s.urgency_evidence),
        ));
        out.push(mk(
            AiAttributeKey::FrustrationCues,
            s.frustration.to_string(),
            s.frustration_confidence,
            evidence_vec(&s.frustration_evidence),
        ));
        out.push(mk(
            AiAttributeKey::TechnicalFamiliarity,
            s.technical.to_string(),
            s.technical_confidence,
            evidence_vec(&s.technical_evidence),
        ));
        // Escalation signal: 'true' carries the evidence; 'false' is low confidence.
        out.push(mk(
            AiAttributeKey::EscalationSignal,
            if s.escalation { "true" } else { "false" }.to_string(),
            if s.escalation { "medium" } else { "low" },
            if s.escalation {
                evidence_vec(&s.escalation_evidence)
            } else {
                Vec::new()
            },
        ));
        // Question count is a deterministic message statistic.
        out.push(mk(
            AiAttributeKey::QuestionCount,
            s.question_count.to_string(),
            "high",
            Vec::new(),
        ));
        if let Some(style) = s.response_style {
            out.push(mk(
                AiAttributeKey::ResponseStyle,
                style.to_string(),
                "high",
                evidence_vec(&s.response_style_evidence),
            ));
        }

        // Risk: composite of OBSERVABLE signals (never a personality claim).
        let risk = if s.escalation
            || (s.urgency == "high" && (s.frustration == "moderate" || s.frustration == "strong"))
        {
            ("high", "medium")
        } else if s.urgency != "none"
            || s.frustration != "none"
            || count_markers(
                &messages
                    .iter()
                    .map(|m| m.text.to_lowercase())
                    .collect::<Vec<_>>()
                    .join("\n"),
                ACTION_EXPECTATION_MARKERS,
            ) >= 1
        {
            ("medium", "low")
        } else {
            ("low", "low")
        };
        let risk_evidence: Vec<Value> = [
            &s.urgency_evidence,
            &s.frustration_evidence,
            &s.escalation_evidence,
        ]
        .into_iter()
        .flatten()
        .take(3)
        .cloned()
        .collect();
        out.push(mk(
            AiAttributeKey::Risk,
            risk.0.to_string(),
            risk.1,
            risk_evidence,
        ));

        if let Some(goal) = &s.customer_goal {
            out.push(mk(
                AiAttributeKey::CustomerGoal,
                goal.clone(),
                "low",
                Vec::new(),
            ));
        }
    }

    // Known issue membership: a stored local fact (always known, 'true'/'false').
    let linked: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM known_issue_links WHERE conversation_id = ?1 LIMIT 1",
            params![conversation_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    out.push(AttributeRecord {
        key: AiAttributeKey::KnownIssue,
        value: if linked.is_some() { "true" } else { "false" }.to_string(),
        confidence: "high",
        source: "deterministic",
        evidence: Vec::new(),
        run_id: None,
    });

    // Issue cluster membership: derived from stored cluster rows.
    let cluster: Option<String> = conn
        .query_row(
            "SELECT ic.name FROM issue_cluster_members icm
             JOIN issue_clusters ic ON ic.id = icm.cluster_id
             WHERE icm.conversation_id = ?1
             ORDER BY ic.id DESC LIMIT 1",
            params![conversation_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    if let Some(name) = cluster {
        out.push(AttributeRecord {
            key: AiAttributeKey::IssueCluster,
            value: name.chars().take(300).collect(),
            confidence: "medium",
            source: "deterministic",
            evidence: Vec::new(),
            run_id: None,
        });
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_provider::FakeAiProvider;
    use crate::ai_provider::NoopAiProvider;
    use rusqlite::Connection;

    /// In-memory DB with everything the attribute layer touches.
    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE app_state (id INTEGER PRIMARY KEY, schema_version INTEGER);
             INSERT INTO app_state (id, schema_version) VALUES (1, 33);
             CREATE TABLE conversations (
                id INTEGER PRIMARY KEY, remote_id INTEGER NOT NULL UNIQUE,
                number INTEGER, subject TEXT, preview TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                mailbox_id INTEGER NOT NULL, assignee_id INTEGER, customer_id INTEGER NOT NULL,
                priority TEXT, created_at TEXT, updated_at TEXT, closed_at TEXT,
                local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
             CREATE TABLE conversation_threads (
                id INTEGER PRIMARY KEY AUTOINCREMENT, conversation_id INTEGER NOT NULL,
                thread_type TEXT NOT NULL, body TEXT, actor_type TEXT NOT NULL,
                actor_id INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE known_issues (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active', description TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE known_issue_links (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                known_issue_id INTEGER NOT NULL, conversation_id INTEGER NOT NULL,
                link_type TEXT NOT NULL DEFAULT 'related');
             CREATE TABLE issue_clusters (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                conversation_count INTEGER NOT NULL DEFAULT 0,
                first_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                status TEXT NOT NULL DEFAULT 'active');
             CREATE TABLE issue_cluster_members (
                cluster_id INTEGER NOT NULL, conversation_id INTEGER NOT NULL,
                PRIMARY KEY (cluster_id, conversation_id));",
        )
        .expect("schema");
        crate::embeddings::apply_m008(&conn).expect("ai_runs");
        apply_m033(&conn).expect("m033");
        conn
    }

    /// DB that still carries the OLD M010 shape, with legacy rows.
    fn legacy_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE app_state (id INTEGER PRIMARY KEY, schema_version INTEGER);
             INSERT INTO app_state (id, schema_version) VALUES (1, 10);
             CREATE TABLE conversations (
                id INTEGER PRIMARY KEY, remote_id INTEGER NOT NULL UNIQUE,
                number INTEGER, subject TEXT, status TEXT NOT NULL DEFAULT 'active',
                mailbox_id INTEGER NOT NULL, customer_id INTEGER NOT NULL);
             CREATE TABLE ai_attributes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER NOT NULL,
                attribute_key TEXT NOT NULL,
                value TEXT NOT NULL,
                evidence_excerpt TEXT NOT NULL,
                thread_ref TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.0,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
             CREATE INDEX idx_ai_attributes_conv ON ai_attributes (conversation_id, attribute_key);
             INSERT INTO ai_attributes (conversation_id, attribute_key, value,
                 evidence_excerpt, thread_ref, confidence)
             VALUES
                (1, 'intent', 'billing', 'I need a refund', 'msg_1', 0.9),
                (1, 'urgency', 'high', 'this is urgent', 'msg_2', 0.0),
                (1, 'legacy_unknown_key', 'x', 'e', 'm', 0.5);",
        )
        .expect("legacy schema");
        conn
    }

    fn insert_conversation(conn: &Connection, id: i64, number: i64, subject: &str) {
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id)
             VALUES (?1, ?1, ?2, ?3, 1, 1)",
            params![id, number, subject],
        )
        .unwrap();
    }

    fn insert_customer_thread(conn: &Connection, conv: i64, body: &str) {
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, thread_type, body, actor_type)
             VALUES (?1, 'customer', ?2, 'customer')",
            params![conv, body],
        )
        .unwrap();
    }

    fn rec(key: AiAttributeKey, value: &str, confidence: &'static str) -> AttributeRecord {
        AttributeRecord {
            key,
            value: value.to_string(),
            confidence,
            source: "deterministic",
            evidence: Vec::new(),
            run_id: None,
        }
    }

    // ---- M033 migration ----------------------------------------------------

    #[test]
    fn m033_creates_reference_shape_on_fresh_db() {
        let conn = fresh_db();
        let cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(ai_attributes)").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|c| c.ok())
                .collect()
        };
        for col in [
            "id",
            "conversation_id",
            "attribute",
            "value",
            "value_type",
            "confidence",
            "source",
            "evidence",
            "run_id",
            "schema_version",
            "computed_at",
            "superseded_at",
        ] {
            assert!(cols.iter().any(|c| c == col), "missing column {col}");
        }
        for idx in [
            "idx_ai_attributes_current",
            "idx_ai_attributes_value",
            "idx_ai_attributes_run",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name = ?1",
                    params![idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing index {idx}");
        }
    }

    #[test]
    fn m033_is_idempotent() {
        let conn = fresh_db();
        apply_m033(&conn).unwrap();
        apply_m033(&conn).unwrap();
    }

    #[test]
    fn m033_migrates_legacy_rows_and_drops_the_old_table() {
        let conn = legacy_db();
        apply_m033(&conn).unwrap();

        // Old table gone.
        let old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='ai_attributes_m010'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 0);

        // Legacy rows migrated: catalog keys kept, unknown key dropped.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_attributes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);

        let (value, value_type, confidence, source, schema_version, evidence): (
            String,
            String,
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT value, value_type, confidence, source, schema_version, evidence
                 FROM ai_attributes WHERE attribute = 'intent'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(value, "billing");
        assert_eq!(value_type, "enum");
        assert_eq!(confidence, "medium"); // confidence 0.9 > 0
        assert_eq!(source, "ai");
        assert_eq!(schema_version, "attributes_v1");
        let parsed: Value = serde_json::from_str(&evidence).unwrap();
        assert_eq!(parsed[0]["excerpt"], "I need a refund");
        assert_eq!(parsed[0]["thread_ref"], "msg_1");

        // confidence 0.0 → 'unknown'.
        let (confidence, computed_present): (String, i64) = conn
            .query_row(
                "SELECT confidence, computed_at IS NOT NULL FROM ai_attributes
                 WHERE attribute = 'urgency'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(confidence, "unknown");
        assert_eq!(computed_present, 1);
    }

    #[test]
    fn m033_migration_is_forward_only_and_repeatable() {
        let conn = legacy_db();
        apply_m033(&conn).unwrap();
        apply_m033(&conn).unwrap(); // second run: no-op path
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_attributes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "rows not duplicated on re-run");
    }

    // ---- save_snapshot -----------------------------------------------------

    #[test]
    fn save_snapshot_retires_current_rows_and_inserts_new_ones() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Urgency, "high", "medium")],
            None,
        )
        .unwrap();
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Risk, "low", "low")],
            None,
        )
        .unwrap();

        let current = current_for_conversation(&conn, 1).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].attribute, "risk");

        // History preserved: urgency row superseded but still present.
        let history = history(&conn, 1, AiAttributeKey::Urgency, 50).unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].superseded_at.is_some());
    }

    #[test]
    fn save_snapshot_drops_catalog_violations() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        let records = vec![
            rec(AiAttributeKey::Intent, "not_in_vocab", "high"), // enum violation
            rec(AiAttributeKey::Urgency, "high", "medium"),      // ok
            rec(AiAttributeKey::QuestionCount, "nan", "high"),   // non-finite number
            rec(AiAttributeKey::QuestionCount, "3", "high"),     // ok
            rec(AiAttributeKey::KnownIssue, "yes", "high"),      // boolean violation
            rec(AiAttributeKey::KnownIssue, "true", "high"),     // ok
        ];
        save_snapshot(&mut conn, 1, &records, None).unwrap();
        let current = current_for_conversation(&conn, 1).unwrap();
        // The reference reads through the (conversation_id, attribute,
        // superseded_at) index with no ORDER BY, so rows arrive attribute-
        // alphabetical — assert on membership, not insertion order.
        let mut keys: Vec<&str> = current.iter().map(|r| r.attribute.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["known_issue", "question_count", "urgency"]);
        let find = |k: &str| {
            current
                .iter()
                .find(|r| r.attribute == k)
                .map(|r| r.value.as_str())
        };
        assert_eq!(find("question_count"), Some("3"));
        assert_eq!(find("known_issue"), Some("true"));
        assert_eq!(find("urgency"), Some("high"));
    }

    #[test]
    fn upsert_single_replaces_only_its_own_key() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        let id1 = upsert_single(
            &mut conn,
            1,
            rec(AiAttributeKey::Intent, "billing", "medium"),
        )
        .unwrap();
        let id2 =
            upsert_single(&mut conn, 1, rec(AiAttributeKey::Urgency, "high", "medium")).unwrap();
        assert!(id1 > 0);
        assert!(id2 > 0);

        // Re-set intent: urgency's current row must survive.
        let id3 = upsert_single(
            &mut conn,
            1,
            rec(AiAttributeKey::Intent, "question", "high"),
        )
        .unwrap();
        let current = current_for_conversation(&conn, 1).unwrap();
        let find = |k: &str| {
            current
                .iter()
                .find(|r| r.attribute == k)
                .map(|r| r.value.as_str())
        };
        assert_eq!(find("intent"), Some("question"));
        assert_eq!(find("urgency"), Some("high"), "sibling key untouched");
        // The previous intent value is retired, not deleted (versioning).
        let history = history(&conn, 1, AiAttributeKey::Intent, 50).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history[0].superseded_at.is_none());
        assert!(history[1].superseded_at.is_some());
        assert!(id3 > 0);

        // Catalog violations are dropped by upsert too (id 0, nothing stored).
        let id4 =
            upsert_single(&mut conn, 1, rec(AiAttributeKey::KnownIssue, "yes", "high")).unwrap();
        assert_eq!(id4, 0);
        assert_eq!(find("known_issue"), None);
    }

    #[test]
    fn save_snapshot_caps_evidence_at_4000_chars() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        let long: String = "x".repeat(9000);
        save_snapshot(
            &mut conn,
            1,
            &[AttributeRecord {
                key: AiAttributeKey::Product,
                value: "p".into(),
                confidence: "medium",
                source: "ai",
                evidence: vec![json!({ "excerpt": long })],
                run_id: None,
            }],
            None,
        )
        .unwrap();
        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM ai_attributes WHERE attribute = 'product'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(evidence.chars().count() <= 4000);
    }

    // ---- snapshot + unknown honesty ----------------------------------------

    #[test]
    fn snapshot_lists_unknown_keys_honestly() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Urgency, "high", "medium")],
            None,
        )
        .unwrap();
        let snap = snapshot(&conn, 1).unwrap().unwrap();
        assert_eq!(snap.conversation_id, 1);
        assert_eq!(snap.conversation_number, Some(101));
        assert_eq!(snap.attributes.len(), 1);
        assert_eq!(snap.attributes[0]["status"], "known");
        assert!(snap.unknown.contains(&"intent"));
        assert!(!snap.unknown.contains(&"urgency"));
        assert!(snap.computed_at.is_some());
    }

    #[test]
    fn snapshot_none_for_missing_or_deleted_conversation() {
        let conn = fresh_db();
        assert!(snapshot(&conn, 999).unwrap().is_none());
        insert_conversation(&conn, 1, 101, "T1");
        conn.execute(
            "UPDATE conversations SET status = 'deleted' WHERE id = 1",
            [],
        )
        .unwrap();
        assert!(snapshot(&conn, 1).unwrap().is_none());
    }

    // ---- history ------------------------------------------------------------

    #[test]
    fn history_returns_newest_first_with_superseded_at() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Urgency, "none", "low")],
            None,
        )
        .unwrap();
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Urgency, "moderate", "medium")],
            None,
        )
        .unwrap();
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Urgency, "high", "medium")],
            None,
        )
        .unwrap();
        let history = history(&conn, 1, AiAttributeKey::Urgency, 50).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].value, "high"); // newest first
        assert!(history[0].superseded_at.is_none()); // current
        assert!(history[2].superseded_at.is_some()); // retired
    }

    // ---- conversations_matching ---------------------------------------------

    #[test]
    fn matching_equals_not_equals_and_contains() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "Alpha");
        insert_conversation(&conn, 2, 102, "Beta");
        save_snapshot(
            &mut conn,
            1,
            &[AttributeRecord {
                key: AiAttributeKey::Product,
                value: "Payments".into(),
                confidence: "medium",
                source: "ai",
                evidence: vec![],
                run_id: None,
            }],
            None,
        )
        .unwrap();
        save_snapshot(
            &mut conn,
            2,
            &[AttributeRecord {
                key: AiAttributeKey::Product,
                value: "Calendar".into(),
                confidence: "low",
                source: "ai",
                evidence: vec![],
                run_id: None,
            }],
            None,
        )
        .unwrap();

        // equals is case-insensitive on stored values.
        let hits = conversations_matching(
            &conn,
            AiAttributeKey::Product,
            MatchOp::Equals,
            "payments",
            200,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, 1);
        assert_eq!(hits[0].number, 101);
        assert_eq!(hits[0].subject.as_deref(), Some("Alpha"));
        assert_eq!(hits[0].value, "Payments");

        let hits = conversations_matching(
            &conn,
            AiAttributeKey::Product,
            MatchOp::NotEquals,
            "payments",
            200,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, 2);

        let hits =
            conversations_matching(&conn, AiAttributeKey::Product, MatchOp::Contains, "AY", 200)
                .unwrap();
        // LIKE is ASCII-case-insensitive, so the raw-case pattern 'AY' matches
        // the lowercased column value 'payments' (which contains 'ay') — and
        // 'calendar' does not contain 'ay' at all.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, 1);
    }

    #[test]
    fn matching_contains_escapes_like_wildcards() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        insert_conversation(&conn, 2, 102, "T2");
        let product = |value: &str| {
            vec![AttributeRecord {
                key: AiAttributeKey::Product,
                value: value.into(),
                confidence: "low",
                source: "ai",
                evidence: vec![],
                run_id: None,
            }]
        };
        save_snapshot(&mut conn, 1, &product("a_b%c"), None).unwrap();
        save_snapshot(&mut conn, 2, &product("axbc"), None).unwrap();
        // '%' is literal: only the value that actually contains a '%' matches
        // (an unescaped '%%%' pattern would match every row, incl. 'axbc').
        let hits =
            conversations_matching(&conn, AiAttributeKey::Product, MatchOp::Contains, "%", 200)
                .unwrap();
        assert_eq!(hits.len(), 1, "escaped % matches only the literal %");
        assert_eq!(hits[0].conversation_id, 1);
        // '_' is literal: 'a_b' must not match 'axbc'.
        let hits = conversations_matching(
            &conn,
            AiAttributeKey::Product,
            MatchOp::Contains,
            "a_b",
            200,
        )
        .unwrap();
        assert_eq!(hits.len(), 1, "escaped _ is literal");
        assert_eq!(hits[0].conversation_id, 1);
        // Sanity: a plain substring still matches normally.
        let hits = conversations_matching(
            &conn,
            AiAttributeKey::Product,
            MatchOp::Contains,
            "axb",
            200,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, 2);
    }

    #[test]
    fn matching_number_ops() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        insert_conversation(&conn, 2, 102, "T2");
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::QuestionCount, "2", "high")],
            None,
        )
        .unwrap();
        save_snapshot(
            &mut conn,
            2,
            &[rec(AiAttributeKey::QuestionCount, "5", "high")],
            None,
        )
        .unwrap();

        let gt =
            conversations_matching(&conn, AiAttributeKey::QuestionCount, MatchOp::Gt, "2", 200)
                .unwrap();
        assert_eq!(gt.len(), 1);
        assert_eq!(gt[0].conversation_id, 2);

        let lte =
            conversations_matching(&conn, AiAttributeKey::QuestionCount, MatchOp::Lte, "2", 200)
                .unwrap();
        assert_eq!(lte.len(), 1);
        assert_eq!(lte[0].conversation_id, 1);

        // Non-finite target matches nothing.
        let nan = conversations_matching(
            &conn,
            AiAttributeKey::QuestionCount,
            MatchOp::Gt,
            "abc",
            200,
        )
        .unwrap();
        assert!(nan.is_empty());
    }

    #[test]
    fn matching_unknown_op_lists_conversations_without_a_current_row() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        insert_conversation(&conn, 2, 102, "T2");
        insert_conversation(&conn, 3, 103, "T3");
        conn.execute(
            "UPDATE conversations SET status = 'deleted' WHERE id = 3",
            [],
        )
        .unwrap();
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Intent, "bug_report", "medium")],
            None,
        )
        .unwrap();

        let hits = conversations_matching(&conn, AiAttributeKey::Intent, MatchOp::Unknown, "", 200)
            .unwrap();
        assert_eq!(hits.len(), 1, "only conv 2 (deleted 3 excluded)");
        assert_eq!(hits[0].conversation_id, 2);
        assert_eq!(hits[0].value, "");
        assert_eq!(hits[0].confidence, "unknown");
        assert_eq!(hits[0].source, "none");
        assert_eq!(hits[0].computed_at, "");
    }

    #[test]
    fn matching_orders_by_conversation_number_and_caps_limit() {
        let mut conn = fresh_db();
        for (id, number) in [(1, 105), (2, 101), (3, 103)] {
            insert_conversation(&conn, id, number, "T");
            save_snapshot(
                &mut conn,
                id,
                &[rec(AiAttributeKey::Risk, "low", "low")],
                None,
            )
            .unwrap();
        }
        let hits =
            conversations_matching(&conn, AiAttributeKey::Risk, MatchOp::Equals, "low", 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].number, 101);
        assert_eq!(hits[1].number, 103);
    }

    #[test]
    fn match_op_allowlists_match_the_reference() {
        use AttributeValueType as T;
        assert!(MatchOp::allowed(T::Number).contains(&MatchOp::Gt));
        assert!(!MatchOp::allowed(T::Enum).contains(&MatchOp::Gt));
        assert!(MatchOp::allowed(T::Enum).contains(&MatchOp::Contains));
        assert!(!MatchOp::allowed(T::Number).contains(&MatchOp::Contains));
        // Invalid op falls back to equals.
        assert_eq!(MatchOp::parse("bogus", T::Enum), MatchOp::Equals);
        assert_eq!(MatchOp::parse("contains", T::Enum), MatchOp::Contains);
        assert_eq!(MatchOp::parse("contains", T::Number), MatchOp::Equals);
    }

    // ---- distributions + distinct_values ------------------------------------

    #[test]
    fn distributions_cover_all_14_keys_with_honest_unknowns() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        insert_conversation(&conn, 2, 102, "T2");
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Urgency, "high", "medium")],
            None,
        )
        .unwrap();
        save_snapshot(
            &mut conn,
            2,
            &[rec(AiAttributeKey::Urgency, "high", "medium")],
            None,
        )
        .unwrap();

        let dists = distributions(&conn).unwrap();
        assert_eq!(dists.len(), 14);
        let urgency = &dists
            .iter()
            .find(|d| d.attribute == "urgency")
            .expect("urgency present");
        assert_eq!(urgency.label, "Urgency");
        assert_eq!(urgency.value_type, "enum");
        assert_eq!(urgency.total_conversations, 2);
        assert_eq!(urgency.known, 2);
        assert_eq!(urgency.unknown, 0);
        assert_eq!(
            urgency.values,
            vec![AttributeValueCount {
                value: "high".into(),
                count: 2
            }]
        );
        let intent = &dists.iter().find(|d| d.attribute == "intent").unwrap();
        assert_eq!(intent.known, 0);
        assert_eq!(intent.unknown, 2);
        assert!(intent.values.is_empty());
    }

    #[test]
    fn distinct_values_are_ordered_and_deduped() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        insert_conversation(&conn, 2, 102, "T2");
        insert_conversation(&conn, 3, 103, "T3");
        for (id, product) in [(1, "beta"), (2, "alpha"), (3, "beta")] {
            save_snapshot(
                &mut conn,
                id,
                &[AttributeRecord {
                    key: AiAttributeKey::Product,
                    value: product.into(),
                    confidence: "low",
                    source: "ai",
                    evidence: vec![],
                    run_id: None,
                }],
                None,
            )
            .unwrap();
        }
        let values = distinct_values(&conn, AiAttributeKey::Product, 50).unwrap();
        assert_eq!(values, vec!["alpha", "beta"]);
        let limited = distinct_values(&conn, AiAttributeKey::Product, 1).unwrap();
        assert_eq!(limited, vec!["alpha"]);
    }

    // ---- deterministic layer -------------------------------------------------

    #[test]
    fn deterministic_records_derive_observable_signals() {
        let conn = fresh_db();
        insert_conversation(&conn, 1, 101, "Prod down");
        insert_customer_thread(
            &conn,
            1,
            "This is urgent - production is down and customers cannot pay! When will this be fixed?",
        );
        insert_customer_thread(
            &conn,
            1,
            "Please reply as soon as possible. We want to escalate this to your manager right now.",
        );

        let records = deterministic_records(&conn, 1).unwrap();
        let find = |k: AiAttributeKey| {
            records
                .iter()
                .find(|r| r.key == k)
                .map(|r| (r.value.as_str(), r.confidence, r.source))
        };

        assert_eq!(
            find(AiAttributeKey::Urgency),
            Some(("high", "medium", "deterministic"))
        );
        assert_eq!(
            find(AiAttributeKey::EscalationSignal),
            Some(("true", "medium", "deterministic"))
        );
        let (qc, _, _) = find(AiAttributeKey::QuestionCount).expect("question_count present");
        assert_eq!(qc, "1");
        assert_eq!(
            find(AiAttributeKey::KnownIssue),
            Some(("false", "high", "deterministic"))
        );
        // Risk composite: urgency high + escalation → high.
        assert_eq!(find(AiAttributeKey::Risk).map(|r| r.0), Some("high"));
    }

    #[test]
    fn deterministic_records_are_enum_closed() {
        let conn = fresh_db();
        insert_conversation(&conn, 1, 101, "Angry API user");
        insert_customer_thread(
            &conn,
            1,
            "The API endpoint and webhook log show a 500 again. Still not working, this is \
             ridiculous and seriously unacceptable. Nobody has fixed the json payload issue.",
        );
        let records = deterministic_records(&conn, 1).unwrap();
        let find = |k: AiAttributeKey| {
            records
                .iter()
                .find(|r| r.key == k)
                .map(|r| r.value.as_str())
        };
        // Every enum-valued record must be inside its closed vocabulary.
        for r in &records {
            if r.key.value_type() == AttributeValueType::Enum {
                assert!(
                    r.key.values().contains(&r.value.as_str()),
                    "{} not in {:?} for {}",
                    r.value,
                    r.key.values(),
                    r.key.as_str()
                );
            }
        }
        assert_eq!(find(AiAttributeKey::Urgency), Some("none"));
        assert_eq!(find(AiAttributeKey::FrustrationCues), Some("strong"));
        // 7 distinct technical markers (api, endpoint, webhook, log, 500,
        // json, payload) → highly_technical.
        assert_eq!(
            find(AiAttributeKey::TechnicalFamiliarity),
            Some("highly_technical")
        );
    }

    #[test]
    fn word_boundary_matching_prevents_substring_false_positives() {
        assert!(has_marker("this is urgent", "urgent"));
        assert!(!has_marker("we were against the idea", "again"));
        assert!(has_marker("it happened again", "again"));
        assert!(!has_marker("urgently waiting", "urgent"));
        // Unicode text must not panic.
        assert!(!has_marker("café über", "api"));
        assert!(has_marker("call the api now", "api"));
    }

    // ---- compute (deterministic half; AI off) --------------------------------

    #[tokio::test]
    async fn compute_rejects_unknown_conversations() {
        let mut conn = fresh_db();
        let provider = NoopAiProvider;
        let snap = compute(&mut conn, &provider, "m", 999, false)
            .await
            .unwrap();
        assert!(snap.is_none());
    }

    #[tokio::test]
    async fn compute_persists_deterministic_snapshot_with_zero_ai() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "Urgent bug");
        insert_customer_thread(
            &conn,
            1,
            "This is urgent, production is down! When will this be fixed? Please fix it now.",
        );
        let provider = NoopAiProvider;
        let snap = compute(&mut conn, &provider, "m", 1, false)
            .await
            .unwrap()
            .expect("snapshot");
        let find = |k: &str| {
            snap.attributes
                .iter()
                .find(|a| a["attribute"] == k)
                .map(|a| a["value"].as_str().unwrap().to_string())
        };
        assert_eq!(find("urgency").as_deref(), Some("high"));
        assert_eq!(find("known_issue").as_deref(), Some("false"));
        assert_eq!(find("question_count").as_deref(), Some("1"));
        assert!(snap.unknown.contains(&"intent"));
        assert!(snap.unknown.contains(&"product"));
    }

    #[tokio::test]
    async fn compute_is_versioned_on_recompute() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T");
        insert_customer_thread(&conn, 1, "Hello, quick question about exports?");
        let provider = NoopAiProvider;
        compute(&mut conn, &provider, "m", 1, false).await.unwrap();
        compute(&mut conn, &provider, "m", 1, false).await.unwrap();
        let history = history(&conn, 1, AiAttributeKey::KnownIssue, 50).unwrap();
        assert!(history.len() >= 2, "recompute versions history");
        let current = current_for_conversation(&conn, 1).unwrap();
        assert_eq!(
            current
                .iter()
                .filter(|r| r.attribute == "known_issue")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn compute_ai_layer_fills_only_ai_slots() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "Refund please");
        insert_customer_thread(&conn, 1, "I need a refund for my billing charge.");
        // A scripted provider returning valid extraction JSON.
        struct ExtractionProvider;
        #[async_trait::async_trait]
        impl LocalAiProvider for ExtractionProvider {
            async fn list_models(&self) -> Result<Vec<crate::ai_provider::ModelInfo>> {
                Ok(Vec::new())
            }
            async fn chat(
                &self,
                _model: &str,
                _messages: &[ChatMessage],
            ) -> Result<crate::ai_provider::ChatResponse> {
                Ok(crate::ai_provider::ChatResponse {
                    content: serde_json::json!({
                        "attributes": [
                            {"attribute": "intent", "value": "billing", "confidence": "high",
                             "evidence_excerpt": "I need a refund", "evidence_thread_local_id": 1},
                            {"attribute": "urgency", "value": "high", "confidence": "high"},
                            {"attribute": "intent", "value": "not_a_real_intent", "confidence": "low"}
                        ]
                    })
                    .to_string(),
                    model: "scripted".into(),
                    usage: None,
                    finish_reason: Some("stop".into()),
                })
            }
            async fn embed(
                &self,
                _model: &str,
                _text: &str,
            ) -> Result<crate::ai_provider::EmbedResponse> {
                Ok(crate::ai_provider::EmbedResponse {
                    vector: Vec::new(),
                    dim: 0,
                    model: "scripted".into(),
                    usage: None,
                })
            }
            async fn is_available(&self) -> bool {
                true
            }
        }

        let provider = ExtractionProvider;
        let snap = compute(&mut conn, &provider, "scripted", 1, false)
            .await
            .unwrap()
            .expect("snapshot");

        let find = |k: &str| {
            snap.attributes
                .iter()
                .find(|a| a["attribute"] == k)
                .cloned()
        };
        // AI slot filled with evidence + run_id.
        let intent = find("intent").expect("intent filled");
        assert_eq!(intent["value"], "billing");
        assert_eq!(intent["source"], "ai");
        assert_eq!(intent["confidence"], "high");
        assert_eq!(intent["evidence"][0]["excerpt"], "I need a refund");
        assert!(intent["run_id"].is_number());
        // Deterministic slot untouched by AI (urgency stays 'none').
        let urgency = find("urgency").expect("deterministic urgency");
        assert_eq!(urgency["value"], "none");
        assert_eq!(urgency["source"], "deterministic");
        // Enum-violating intent value never stored.
        assert_eq!(
            snap.attributes
                .iter()
                .filter(|a| a["attribute"] == "intent")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn compute_ai_results_are_cached_by_input_hash() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T");
        insert_customer_thread(&conn, 1, "Same text every time.");
        let provider = FakeAiProvider::new();
        // FakeAiProvider returns non-JSON → nothing stored; run the call twice
        // to ensure the cache path is exercised without error.
        let r1 = compute(&mut conn, &provider, "fake-chat-model", 1, false)
            .await
            .unwrap()
            .expect("snapshot");
        let r2 = compute(&mut conn, &provider, "fake-chat-model", 1, false)
            .await
            .unwrap()
            .expect("snapshot");
        assert_eq!(r1.conversation_id, r2.conversation_id);
    }

    // ---- evidence + helpers ---------------------------------------------------

    #[test]
    fn evidence_excerpt_collapses_whitespace_and_caps() {
        let out = excerpt("  a   b\t\nc  ", 220);
        assert_eq!(out, "a b c");
        let long = "x".repeat(300);
        let out = excerpt(&long, 220);
        assert!(out.chars().count() <= 220);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn js_number_semantics() {
        assert_eq!(parse_js_number(""), Some(0.0));
        assert_eq!(parse_js_number("  7 "), Some(7.0));
        assert_eq!(parse_js_number("3.5"), Some(3.5));
        assert_eq!(parse_js_number("abc"), None);
    }

    #[test]
    fn like_escaping_backslash_percent_underscore() {
        assert_eq!(escape_like(r"a\b"), r"a\\b");
        assert_eq!(escape_like("a%b"), r"a\%b");
        assert_eq!(escape_like("a_b"), r"a\_b");
        assert_eq!(escape_like("plain"), "plain");
    }

    #[test]
    fn conversations_with_attributes_targets_backfill() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1, 101, "T1");
        insert_conversation(&conn, 2, 102, "T2");
        save_snapshot(
            &mut conn,
            1,
            &[rec(AiAttributeKey::Risk, "low", "low")],
            None,
        )
        .unwrap();
        let ids = conversations_with_attributes(&conn, 1000).unwrap();
        assert_eq!(ids, vec![1]);
    }

    #[test]
    fn attribute_row_serializes_reference_shape() {
        let row = AttributeRow {
            id: 1,
            conversation_id: 2,
            conversation_number: Some(101),
            attribute: "intent".into(),
            value: "billing".into(),
            value_type: "enum".into(),
            confidence: "high".into(),
            source: "ai".into(),
            evidence: json!([{ "excerpt": "e", "thread_local_id": 3 }]),
            run_id: Some(9),
            schema_version: "attributes_v1".into(),
            computed_at: "2026-01-01 00:00:00".into(),
            superseded_at: None,
        };
        let s = serde_json::to_string(&row).unwrap();
        assert!(s.contains("\"conversation_number\":101"));
        assert!(s.contains("\"schema_version\":\"attributes_v1\""));
        assert!(
            !s.contains("superseded_at"),
            "the wire shape has no superseded_at (reference mapRow drops it)"
        );
        // Versioned (retired) rows keep the stamp internally but still never
        // serialize it — the reference history response has no such field.
        let mut retired = row;
        retired.superseded_at = Some("2026-01-02 00:00:00".into());
        let s = serde_json::to_string(&retired).unwrap();
        assert!(!s.contains("superseded_at"));
        assert!(retired.superseded_at.is_some());
    }
}
