//! AI features — verified drafts, coaching, customer memory, translation,
//! post-resolution QA, suggestions (M6-T05 through M6-T10).
//!
//! Per spec M6: "verified drafts, coaching, customer memory, translation, QA,
//! suggestions."
//! Per spec: "AI is always advisory. Automatic customer-reply sending is
//! permanently OFF. 'Unknown' is a legitimate answer; never fabricate values."
//! Per spec A1: "Ticket translation (spec section 65) IS included as a
//! SupportOS feature."
//! Per the reference notes: "preference requires ≥3 observations before
//! counting as a pattern."

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

// ─── M009–M013 migrations ────────────────────────────────────────────────

/// M011 migration: verified_drafts table.
pub const M011_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS verified_drafts (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL,
        draft_text      TEXT NOT NULL,
        status          TEXT NOT NULL DEFAULT 'pending',
        created_by_ai_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        approved_by_user_id INTEGER,
        approved_at     TEXT,
        sent_at         TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_verified_drafts_conv
        ON verified_drafts (conversation_id, status);
"#;

/// M012 migration: customer_memory table.
pub const M012_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS customer_memory (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        customer_id     INTEGER NOT NULL,
        memory_key      TEXT NOT NULL,
        memory_value    TEXT NOT NULL,
        evidence_excerpt TEXT NOT NULL,
        source_conversation_id INTEGER,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_customer_memory_customer
        ON customer_memory (customer_id, memory_key);
"#;

/// M013 migration: post_resolution_qa table.
pub const M013_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS post_resolution_qa (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL,
        qa_score        REAL,
        qa_notes        TEXT,
        checked_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_post_resolution_qa_conv
        ON post_resolution_qa (conversation_id);
"#;

/// Apply M011–M013 migrations. Idempotent.
pub fn apply_m011_to_m013(conn: &Connection) -> Result<()> {
    conn.execute_batch(M011_SQL)?;
    conn.execute_batch(M012_SQL)?;
    conn.execute_batch(M013_SQL)?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 13 WHERE id = 1", []);
    Ok(())
}

// ─── M6-T05: Verified drafts ────────────────────────────────────────────

/// The status of a verified draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    Pending,
    Approved,
    Sent,
    Rejected,
}

impl DraftStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Sent => "sent",
            Self::Rejected => "rejected",
        }
    }
}

/// A verified draft row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedDraft {
    pub id: Option<i64>,
    pub conversation_id: i64,
    pub draft_text: String,
    pub status: String,
    pub created_by_ai_at: String,
    pub approved_by_user_id: Option<i64>,
    pub approved_at: Option<String>,
    pub sent_at: Option<String>,
}

/// Create a new AI-drafted reply. Per spec: "Automatic customer-reply sending
/// is permanently OFF." Drafts start as 'pending' — they require human
/// approval before sending.
pub fn create_draft(conn: &Connection, conversation_id: i64, draft_text: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO verified_drafts (conversation_id, draft_text, status)
         VALUES (?1, ?2, 'pending')",
        params![conversation_id, draft_text],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Approve a draft. Per spec: the send function exists but is never called
/// automatically — only via explicit human action.
pub fn approve_draft(conn: &Connection, draft_id: i64, user_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE verified_drafts
         SET status = 'approved', approved_by_user_id = ?1,
             approved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?2 AND status = 'pending'",
        params![user_id, draft_id],
    )?;
    Ok(rows > 0)
}

/// Send an approved draft. Per spec: "Automatic customer-reply sending is
/// permanently OFF." This function only succeeds if the draft was previously
/// approved — it NEVER auto-sends.
pub fn send_draft(conn: &Connection, draft_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE verified_drafts
         SET status = 'sent', sent_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1 AND status = 'approved'",
        params![draft_id],
    )?;
    Ok(rows > 0)
}

/// Reject a draft.
pub fn reject_draft(conn: &Connection, draft_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE verified_drafts SET status = 'rejected' WHERE id = ?1 AND status = 'pending'",
        params![draft_id],
    )?;
    Ok(rows > 0)
}

/// List drafts for a conversation.
pub fn list_drafts(conn: &Connection, conversation_id: i64) -> Result<Vec<VerifiedDraft>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, draft_text, status, created_by_ai_at,
                approved_by_user_id, approved_at, sent_at
         FROM verified_drafts WHERE conversation_id = ?1 ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map(params![conversation_id], |r| {
            Ok(VerifiedDraft {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                draft_text: r.get(2)?,
                status: r.get(3)?,
                created_by_ai_at: r.get(4)?,
                approved_by_user_id: r.get(5)?,
                approved_at: r.get(6)?,
                sent_at: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ─── M6-T06: Coaching suggestions ────────────────────────────────────────

/// A coaching suggestion — advisory, never auto-acting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoachingSuggestion {
    pub suggestion: String,
    pub rationale: String,
}

/// Generate coaching suggestions. Per spec: AI is advisory. This function
/// builds a prompt but doesn't call the provider (the caller wires the
/// `LocalAiProvider`). Returns suggestions capped at `MAX_SUGGESTIONS`.
pub const MAX_SUGGESTIONS: usize = 5;

/// Parse coaching suggestions from an AI response. Pure function.
#[must_use]
pub fn parse_coaching_suggestions(response: &str) -> Vec<CoachingSuggestion> {
    #[derive(serde::Deserialize)]
    struct CoachingResponse {
        suggestions: Vec<CoachingSuggestionRaw>,
    }
    #[derive(serde::Deserialize)]
    struct CoachingSuggestionRaw {
        suggestion: String,
        rationale: String,
    }
    let parsed: std::result::Result<CoachingResponse, _> = serde_json::from_str(response);
    match parsed {
        Ok(resp) => resp
            .suggestions
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|s| CoachingSuggestion {
                suggestion: s.suggestion,
                rationale: s.rationale,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ─── M6-T07: Customer memory ────────────────────────────────────────────

/// A customer memory entry — AI-derived, never overwriting Help Scout data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomerMemory {
    pub id: Option<i64>,
    pub customer_id: i64,
    pub memory_key: String,
    pub memory_value: String,
    pub evidence_excerpt: String,
    pub source_conversation_id: Option<i64>,
    pub created_at: String,
}

/// Store a customer memory. Per spec: AI-derived, never overwriting Help
/// Scout source data.
pub fn set_memory(
    conn: &Connection,
    customer_id: i64,
    key: &str,
    value: &str,
    evidence: &str,
    source_conv: Option<i64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO customer_memory (customer_id, memory_key, memory_value, evidence_excerpt, source_conversation_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![customer_id, key, value, evidence, source_conv],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all memory entries for a customer.
pub fn get_memory(conn: &Connection, customer_id: i64) -> Result<Vec<CustomerMemory>> {
    let mut stmt = conn.prepare(
        "SELECT id, customer_id, memory_key, memory_value, evidence_excerpt, source_conversation_id, created_at
         FROM customer_memory WHERE customer_id = ?1 ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map(params![customer_id], |r| {
            Ok(CustomerMemory {
                id: r.get(0)?,
                customer_id: r.get(1)?,
                memory_key: r.get(2)?,
                memory_value: r.get(3)?,
                evidence_excerpt: r.get(4)?,
                source_conversation_id: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Count memory entries for a specific key. Per the reference notes:
/// "preference requires ≥3 observations before counting as a pattern."
pub fn count_memory_entries(conn: &Connection, customer_id: i64, key: &str) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM customer_memory WHERE customer_id = ?1 AND memory_key = ?2",
        params![customer_id, key],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

/// Whether a memory key qualifies as a pattern (≥3 observations).
/// Per the reference notes: INTERACTION_MIN_OBSERVATIONS_FOR_PREFERENCE = 3.
pub const MIN_OBSERVATIONS_FOR_PREFERENCE: u32 = 3;

/// Check if a memory key is a confirmed pattern (≥3 observations).
pub fn is_confirmed_pattern(conn: &Connection, customer_id: i64, key: &str) -> Result<bool> {
    Ok(count_memory_entries(conn, customer_id, key)? >= MIN_OBSERVATIONS_FOR_PREFERENCE)
}

// ─── M6-T08: Translation ─────────────────────────────────────────────────

/// The prompt version for translation caching.
pub const TRANSLATION_PROMPT_VERSION: &str = "translation-v1";

/// Build a translation prompt. Pure function.
#[must_use]
pub fn build_translation_prompt(text: &str, target_lang: &str) -> String {
    format!(
        "Translate the following text to {target_lang}. Provide ONLY the translation, no explanation.\n\n{text}"
    )
}

// ─── M6-T09: Post-resolution QA ──────────────────────────────────────────

/// A QA result — quality checks after a conversation is closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QaResult {
    pub conversation_id: i64,
    pub qa_score: Option<f64>,
    pub qa_notes: String,
    pub from_cache: bool,
}

/// Store a QA result.
pub fn store_qa(
    conn: &Connection,
    conversation_id: i64,
    score: Option<f64>,
    notes: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO post_resolution_qa (conversation_id, qa_score, qa_notes)
         VALUES (?1, ?2, ?3)",
        params![conversation_id, score, notes],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get the latest QA result for a conversation.
pub fn get_qa(conn: &Connection, conversation_id: i64) -> Result<Option<QaResult>> {
    let row: Option<(Option<f64>, String)> = conn
        .query_row(
            "SELECT qa_score, qa_notes FROM post_resolution_qa
             WHERE conversation_id = ?1 ORDER BY checked_at DESC LIMIT 1",
            params![conversation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    match row {
        None => Ok(None),
        Some((score, notes)) => Ok(Some(QaResult {
            conversation_id,
            qa_score: score,
            qa_notes: notes,
            from_cache: false,
        })),
    }
}

// ─── M6-T10: Suggestions ─────────────────────────────────────────────────

/// A proactive suggestion surfaced to the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub suggestion: String,
    pub category: String,
}

/// Parse suggestions from an AI response. Pure function.
#[must_use]
pub fn parse_suggestions(response: &str) -> Vec<Suggestion> {
    #[derive(serde::Deserialize)]
    struct SuggestionsResponse {
        suggestions: Vec<SuggestionRaw>,
    }
    #[derive(serde::Deserialize)]
    struct SuggestionRaw {
        suggestion: String,
        category: String,
    }
    let parsed: std::result::Result<SuggestionsResponse, _> = serde_json::from_str(response);
    match parsed {
        Ok(resp) => resp
            .suggestions
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|s| Suggestion {
                suggestion: s.suggestion,
                category: s.category,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
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
        apply_m011_to_m013(&conn).unwrap();
        conn
    }

    // ---- M011–M013 migrations ----------------------------------------------

    #[test]
    fn m011_creates_verified_drafts_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM verified_drafts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m012_creates_customer_memory_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM customer_memory", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m013_creates_post_resolution_qa_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM post_resolution_qa", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m011_to_m013_is_idempotent() {
        let conn = fresh_db();
        apply_m011_to_m013(&conn).unwrap();
    }

    // ---- M6-T05: Verified drafts lifecycle --------------------------------

    #[test]
    fn draft_lifecycle_pending_to_approved_to_sent() {
        let conn = fresh_db();
        let id = create_draft(&conn, 1001, "Hello, here's your refund.").unwrap();

        // Verify pending.
        let drafts = list_drafts(&conn, 1001).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].status, "pending");

        // Approve.
        assert!(approve_draft(&conn, id, 42).unwrap());
        let drafts = list_drafts(&conn, 1001).unwrap();
        assert_eq!(drafts[0].status, "approved");
        assert_eq!(drafts[0].approved_by_user_id, Some(42));

        // Send (only after approval).
        assert!(send_draft(&conn, id).unwrap());
        let drafts = list_drafts(&conn, 1001).unwrap();
        assert_eq!(drafts[0].status, "sent");
        assert!(drafts[0].sent_at.is_some());
    }

    #[test]
    fn send_draft_rejected_without_approval() {
        let conn = fresh_db();
        let id = create_draft(&conn, 1001, "test draft").unwrap();
        // Try to send without approving first.
        assert!(
            !send_draft(&conn, id).unwrap(),
            "send must fail without approval"
        );
    }

    #[test]
    fn approve_draft_rejects_already_approved() {
        let conn = fresh_db();
        let id = create_draft(&conn, 1001, "test").unwrap();
        assert!(approve_draft(&conn, id, 42).unwrap());
        // Second approval returns false.
        assert!(!approve_draft(&conn, id, 43).unwrap());
    }

    #[test]
    fn reject_draft_works_on_pending() {
        let conn = fresh_db();
        let id = create_draft(&conn, 1001, "test").unwrap();
        assert!(reject_draft(&conn, id).unwrap());
        let drafts = list_drafts(&conn, 1001).unwrap();
        assert_eq!(drafts[0].status, "rejected");
    }

    #[test]
    fn draft_status_as_str() {
        assert_eq!(DraftStatus::Pending.as_str(), "pending");
        assert_eq!(DraftStatus::Approved.as_str(), "approved");
        assert_eq!(DraftStatus::Sent.as_str(), "sent");
        assert_eq!(DraftStatus::Rejected.as_str(), "rejected");
    }

    // ---- M6-T06: Coaching suggestions --------------------------------------

    #[test]
    fn parse_coaching_suggestions_valid_json() {
        let json = r#"{"suggestions": [
            {"suggestion": "Acknowledge frustration", "rationale": "Customer seems upset"},
            {"suggestion": "Link to KB", "rationale": "Known issue"}
        ]}"#;
        let suggestions = parse_coaching_suggestions(json);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].suggestion, "Acknowledge frustration");
    }

    #[test]
    fn parse_coaching_suggestions_malformed_returns_empty() {
        assert!(parse_coaching_suggestions("not json").is_empty());
    }

    #[test]
    fn parse_coaching_suggestions_capped_at_5() {
        let mut items = String::from(r#"{"suggestions": ["#);
        for i in 0..10 {
            if i > 0 {
                items.push(',');
            }
            items.push_str(&format!(r#"{{"suggestion": "s{i}", "rationale": "r{i}"}}"#));
        }
        items.push_str("]}");
        let suggestions = parse_coaching_suggestions(&items);
        assert_eq!(
            suggestions.len(),
            MAX_SUGGESTIONS,
            "capped at {MAX_SUGGESTIONS}"
        );
    }

    // ---- M6-T07: Customer memory -------------------------------------------

    #[test]
    fn set_memory_round_trips() {
        let conn = fresh_db();
        set_memory(
            &conn,
            2001,
            "preferred_language",
            "English",
            "Customer writes in English",
            Some(1001),
        )
        .unwrap();
        let memories = get_memory(&conn, 2001).unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_key, "preferred_language");
        assert_eq!(memories[0].memory_value, "English");
        assert_eq!(memories[0].evidence_excerpt, "Customer writes in English");
        assert_eq!(memories[0].source_conversation_id, Some(1001));
    }

    #[test]
    fn memory_is_per_customer() {
        let conn = fresh_db();
        set_memory(&conn, 2001, "key", "val1", "ev1", None).unwrap();
        set_memory(&conn, 2002, "key", "val2", "ev2", None).unwrap();
        assert_eq!(get_memory(&conn, 2001).unwrap().len(), 1);
        assert_eq!(get_memory(&conn, 2002).unwrap().len(), 1);
    }

    #[test]
    fn count_memory_entries_returns_correct_count() {
        let conn = fresh_db();
        assert_eq!(count_memory_entries(&conn, 2001, "lang").unwrap(), 0);
        set_memory(&conn, 2001, "lang", "en", "e1", None).unwrap();
        set_memory(&conn, 2001, "lang", "en", "e2", None).unwrap();
        assert_eq!(count_memory_entries(&conn, 2001, "lang").unwrap(), 2);
    }

    #[test]
    fn is_confirmed_pattern_requires_3_observations() {
        let conn = fresh_db();
        // 0 observations → not a pattern.
        assert!(!is_confirmed_pattern(&conn, 2001, "lang").unwrap());
        // 1 observation → not a pattern.
        set_memory(&conn, 2001, "lang", "en", "e1", None).unwrap();
        assert!(!is_confirmed_pattern(&conn, 2001, "lang").unwrap());
        // 2 observations → not a pattern.
        set_memory(&conn, 2001, "lang", "en", "e2", None).unwrap();
        assert!(!is_confirmed_pattern(&conn, 2001, "lang").unwrap());
        // 3 observations → pattern confirmed.
        set_memory(&conn, 2001, "lang", "en", "e3", None).unwrap();
        assert!(is_confirmed_pattern(&conn, 2001, "lang").unwrap());
    }

    // ---- M6-T08: Translation -----------------------------------------------

    #[test]
    fn build_translation_prompt_includes_text_and_target() {
        let prompt = build_translation_prompt("Hello world", "French");
        assert!(prompt.contains("Hello world"));
        assert!(prompt.contains("French"));
    }

    #[test]
    fn translation_prompt_version_is_v1() {
        assert_eq!(TRANSLATION_PROMPT_VERSION, "translation-v1");
    }

    // ---- M6-T09: Post-resolution QA ----------------------------------------

    #[test]
    fn store_qa_round_trips() {
        let conn = fresh_db();
        store_qa(&conn, 1001, Some(0.85), "Good resolution, clear answer.").unwrap();
        let qa = get_qa(&conn, 1001).unwrap().unwrap();
        assert_eq!(qa.conversation_id, 1001);
        assert!((qa.qa_score.unwrap() - 0.85).abs() < 1e-6);
        assert_eq!(qa.qa_notes, "Good resolution, clear answer.");
    }

    #[test]
    fn get_qa_returns_none_for_nonexistent() {
        let conn = fresh_db();
        assert!(get_qa(&conn, 9999).unwrap().is_none());
    }

    #[test]
    fn get_qa_returns_latest() {
        let conn = fresh_db();
        store_qa(&conn, 1001, Some(0.5), "first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store_qa(&conn, 1001, Some(0.9), "second").unwrap();
        let qa = get_qa(&conn, 1001).unwrap().unwrap();
        assert_eq!(qa.qa_notes, "second", "latest QA result returned");
    }

    // ---- M6-T10: Suggestions -----------------------------------------------

    #[test]
    fn parse_suggestions_valid_json() {
        let json = r#"{"suggestions": [
            {"suggestion": "Escalate to senior", "category": "escalation"},
            {"suggestion": "Link to KB article", "category": "knowledge"}
        ]}"#;
        let suggestions = parse_suggestions(json);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].category, "escalation");
    }

    #[test]
    fn parse_suggestions_malformed_returns_empty() {
        assert!(parse_suggestions("invalid").is_empty());
    }

    #[test]
    fn parse_suggestions_capped_at_5() {
        let mut items = String::from(r#"{"suggestions": ["#);
        for i in 0..10 {
            if i > 0 {
                items.push(',');
            }
            items.push_str(&format!(r#"{{"suggestion": "s{i}", "category": "c{i}"}}"#));
        }
        items.push_str("]}");
        let suggestions = parse_suggestions(&items);
        assert_eq!(suggestions.len(), MAX_SUGGESTIONS);
    }

    // ---- serde -------------------------------------------------------------

    #[test]
    fn verified_draft_serializes() {
        let d = VerifiedDraft {
            id: Some(1),
            conversation_id: 1001,
            draft_text: "Hello".into(),
            status: "pending".into(),
            created_by_ai_at: "2026-10-01T10:00:00Z".into(),
            approved_by_user_id: None,
            approved_at: None,
            sent_at: None,
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"status\":\"pending\""));
    }

    #[test]
    fn coaching_suggestion_serializes() {
        let cs = CoachingSuggestion {
            suggestion: "Acknowledge frustration".into(),
            rationale: "Customer seems upset".into(),
        };
        let s = serde_json::to_string(&cs).unwrap();
        assert!(s.contains("\"suggestion\":\"Acknowledge frustration\""));
    }

    #[test]
    fn customer_memory_serializes() {
        let m = CustomerMemory {
            id: Some(1),
            customer_id: 2001,
            memory_key: "preferred_language".into(),
            memory_value: "English".into(),
            evidence_excerpt: "Customer writes in English".into(),
            source_conversation_id: Some(1001),
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"memory_key\":\"preferred_language\""));
    }

    #[test]
    fn qa_result_serializes() {
        let qa = QaResult {
            conversation_id: 1001,
            qa_score: Some(0.85),
            qa_notes: "Good".into(),
            from_cache: false,
        };
        let s = serde_json::to_string(&qa).unwrap();
        assert!(s.contains("\"qa_score\":0.85"));
    }

    #[test]
    fn suggestion_serializes() {
        let s = Suggestion {
            suggestion: "Escalate".into(),
            category: "escalation".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"category\":\"escalation\""));
    }
}
