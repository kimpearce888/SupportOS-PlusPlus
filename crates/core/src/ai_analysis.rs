//! AI analysis + AI attributes (M6-T02 + M6-T03).
//!
//! Per spec M6: "analysis, attributes."
//! Per spec: "AI is always advisory. 'Unknown' is a legitimate answer; never
//! fabricate values. AI attributes never overwrite Help Scout source data."
//! Per the reference notes: "Every AI-derived attribute carries an evidence
//! excerpt + thread reference."
//!
//! ## Design
//!
//! M6-T02 (analysis): `analyze_conversation` calls `provider.chat()` with a
//! structured prompt asking the AI to analyze the conversation. The result
//! is a structured `AnalysisResult` containing the 14 AI attribute keys.
//! The analysis is cached via the `ai_runs` table (M5-T07).
//!
//! M6-T03 (attributes): the persisted attribute layer now lives in
//! `crate::ai_attributes` (M033 reference shape: versioned snapshots via
//! `superseded_at`, closed 14-key catalog, deterministic + AI layers).
//! `apply_m010` below only remains for boot-order compatibility: it creates
//! the legacy shape on DBs that have not yet reached M033, and is a no-op
//! once the M033 shape is in place.
//!
//! Per spec: AI attributes never overwrite Help Scout source data — they're a
//! separate layer, like `supportos_priority` from M3-T04.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::ai_provider::{ChatMessage, ChatResponse, LocalAiProvider};
use crate::catalog::AiAttributeKey;
use crate::embeddings;
use crate::error::{Error, Result};

/// The M010 migration: creates the `ai_attributes` table.
pub const M010_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS ai_attributes (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL,
        attribute_key   TEXT NOT NULL,
        value           TEXT NOT NULL,
        evidence_excerpt TEXT NOT NULL,
        thread_ref      TEXT NOT NULL,
        confidence      REAL NOT NULL DEFAULT 0.0,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_ai_attributes_conv
        ON ai_attributes (conversation_id, attribute_key);

    UPDATE app_state SET schema_version = 10 WHERE id = 1;
"#;

/// Apply M010 migration. Idempotent. Superseded by M033 (`ai_attributes`
/// reference shape): once the table carries the `attribute` column, this is a
/// deliberate no-op so re-running the boot sequence never recreates the
/// legacy index on the rebuilt table.
pub fn apply_m010(conn: &Connection) -> Result<()> {
    // M033 shape already present — nothing to do (forward-only).
    {
        let mut stmt = conn.prepare("PRAGMA table_info(ai_attributes)")?;
        let has_new_shape = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|c| c.ok())
            .any(|c| c == "attribute");
        if has_new_shape {
            return Ok(());
        }
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ai_attributes (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL,
            attribute_key   TEXT NOT NULL,
            value           TEXT NOT NULL,
            evidence_excerpt TEXT NOT NULL,
            thread_ref      TEXT NOT NULL,
            confidence      REAL NOT NULL DEFAULT 0.0,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_ai_attributes_conv
            ON ai_attributes (conversation_id, attribute_key);",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 10 WHERE id = 1", []);
    Ok(())
}

/// A stored AI attribute — one of the 14 keys with a value, evidence, and
/// thread reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAttribute {
    /// The row id.
    pub id: Option<i64>,
    /// The conversation this attribute belongs to (remote_id).
    pub conversation_id: i64,
    /// The attribute key (one of 14 from the catalog — typed enum).
    pub key: AiAttributeKey,
    /// The attribute value (e.g., "refund", "high", "frustrated").
    /// Per spec: "Unknown" is a legitimate answer.
    pub value: String,
    /// The evidence excerpt — a quote from the conversation that supports
    /// this attribute. Per the reference notes: "Every AI-derived attribute
    /// carries an evidence excerpt + thread reference."
    pub evidence_excerpt: String,
    /// The thread reference — identifies which message in the conversation
    /// the evidence came from.
    pub thread_ref: String,
    /// The confidence score (0.0 to 1.0). AI is advisory; confidence helps
    /// the agent decide whether to trust the attribute.
    pub confidence: f64,
    /// When the attribute was created (ISO-8601 UTC).
    pub created_at: String,
}

/// The result of analyzing a conversation — a set of AI attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// The conversation that was analyzed (remote_id).
    pub conversation_id: i64,
    /// The attributes derived from the analysis.
    pub attributes: Vec<AiAttribute>,
    /// The raw AI response (for debugging/transparency).
    pub raw_response: String,
    /// Whether the analysis was served from cache (true) or freshly computed (false).
    pub from_cache: bool,
}

/// The prompt version for analysis. Per the reference notes: "AI output cached
/// by `hash(input_text) + prompt_version`." Bumping this invalidates the cache.
pub const ANALYSIS_PROMPT_VERSION: &str = "analysis-v1";

/// Analyze a conversation using the local AI provider. Per spec: "AI is always
/// advisory. 'Unknown' is a legitimate answer; never fabricate values."
///
/// The analysis is cached via the `ai_runs` table (M5-T07). The cache key is
/// `(content_hash(conversation_text), ANALYSIS_PROMPT_VERSION, model)` — so
/// the same conversation text + model + prompt version returns the cached
/// result without calling the provider again.
///
/// # Errors
///
/// Returns the provider's error if the chat call fails (after retries), or
/// `Error::Sqlite`/`Error::Config` for cache I/O failures.
pub async fn analyze_conversation(
    conn: &Connection,
    provider: &dyn LocalAiProvider,
    model: &str,
    conversation_text: &str,
    conversation_id: i64,
) -> Result<AnalysisResult> {
    // Build the analysis prompt.
    let prompt = build_analysis_prompt(conversation_text);

    // Safety invariant (spec #127 / reference lmStudioProvider.chatJson with
    // `redact: true`): payment data, tokens and API keys never reach the AI
    // prompt. The setting `redaction_enabled` (default true) toggles the
    // pass, exactly like the reference's `redactionEnabled` read.
    let redaction_enabled =
        crate::settings::get_bool(conn, "redaction_enabled", true).unwrap_or(true);
    let (prompt, _redactions) = crate::security::redact_text(&prompt, redaction_enabled);

    // Check the cache via ai_runs. We use the embeddings cache since it stores
    // arbitrary JSON — the chat response is serialized as JSON.
    let input_hash = embeddings::content_hash(&prompt);
    let cached = lookup_cached_analysis(conn, &input_hash, ANALYSIS_PROMPT_VERSION, model)?;

    if let Some(cached_response) = cached {
        // Cache hit — return the cached result.
        let attributes = parse_analysis_response(&cached_response.content, conversation_id);
        return Ok(AnalysisResult {
            conversation_id,
            attributes,
            raw_response: cached_response.content,
            from_cache: true,
        });
    }

    // Cache miss — call the provider.
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: ANALYSIS_SYSTEM_PROMPT.into(),
        },
        ChatMessage {
            role: "user".into(),
            content: prompt,
        },
    ];

    let response = provider.chat(model, &messages).await?;

    // Store in cache.
    store_cached_analysis(conn, &input_hash, ANALYSIS_PROMPT_VERSION, model, &response)?;

    let attributes = parse_analysis_response(&response.content, conversation_id);
    Ok(AnalysisResult {
        conversation_id,
        attributes,
        raw_response: response.content,
        from_cache: false,
    })
}

/// The system prompt for analysis. Per spec: "AI is always advisory. 'Unknown'
/// is a legitimate answer; never fabricate values."
const ANALYSIS_SYSTEM_PROMPT: &str = "\
You are a support analysis assistant. Analyze the conversation and extract \
AI attributes. For each attribute, provide a value, an evidence excerpt \
(a direct quote from the conversation), and a thread reference (which message \
the evidence came from). If you cannot determine an attribute, use 'Unknown' \
as the value — never fabricate. Respond as JSON with this format:
{\"attributes\": [{\"key\": \"intent\", \"value\": \"refund request\", \
\"evidence\": \"I want my money back\", \"thread_ref\": \"msg_1\", \
\"confidence\": 0.9}]}";

/// Build the user prompt for analysis.
fn build_analysis_prompt(conversation_text: &str) -> String {
    format!("Analyze this support conversation:\n\n{conversation_text}")
}

/// Parse the AI's response into AI attributes. Per spec: "never fabricate" —
/// if the response can't be parsed, returns an empty vec (no attributes).
fn parse_analysis_response(response: &str, conversation_id: i64) -> Vec<AiAttribute> {
    // Try to parse as JSON. If it fails, return empty (per spec: "Unknown"
    // is a legitimate answer, but malformed AI output should not crash).
    #[derive(serde::Deserialize)]
    struct AnalysisResponse {
        attributes: Vec<AnalysisAttribute>,
    }
    #[derive(serde::Deserialize)]
    struct AnalysisAttribute {
        key: String,
        value: String,
        evidence: String,
        thread_ref: String,
        confidence: Option<f64>,
    }

    let parsed: std::result::Result<AnalysisResponse, _> = serde_json::from_str(response);
    match parsed {
        Ok(resp) => resp
            .attributes
            .into_iter()
            .filter_map(|a| {
                // Validate the key against the catalog (14 keys — single source of truth).
                let key = AiAttributeKey::ALL
                    .iter()
                    .find(|k| k.as_str() == a.key)
                    .copied()?;
                Some(AiAttribute {
                    id: None,
                    conversation_id,
                    key,
                    value: a.value,
                    evidence_excerpt: a.evidence,
                    thread_ref: a.thread_ref,
                    confidence: a.confidence.unwrap_or(0.0),
                    created_at: String::new(),
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Store a cached analysis response in the `ai_runs` table.
fn store_cached_analysis(
    conn: &Connection,
    input_hash: &str,
    prompt_version: &str,
    model: &str,
    response: &ChatResponse,
) -> Result<()> {
    let json = serde_json::to_string(response)
        .map_err(|e| Error::Config(format!("analysis cache serialization failed: {e}")))?;
    conn.execute(
        "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![input_hash, prompt_version, model, json],
    )?;
    Ok(())
}

/// Look up a cached analysis response.
fn lookup_cached_analysis(
    conn: &Connection,
    input_hash: &str,
    prompt_version: &str,
    model: &str,
) -> Result<Option<ChatResponse>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT response_json FROM ai_runs
             WHERE input_hash = ?1 AND prompt_version = ?2 AND model = ?3
             ORDER BY created_at DESC LIMIT 1",
            params![input_hash, prompt_version, model],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    match json {
        None => Ok(None),
        Some(j) => {
            let resp: ChatResponse = serde_json::from_str(&j).map_err(|e| {
                Error::Config(format!("analysis cache deserialization failed: {e}"))
            })?;
            Ok(Some(resp))
        }
    }
}

// ---------------------------------------------------------------------------
// Attribute persistence
// ---------------------------------------------------------------------------

/// Store one derived AI attribute via the versioned attribute repo
/// (`crate::ai_attributes::upsert_single`: retires only this key's current
/// rows — sibling keys stay untouched — then inserts through the closed
/// catalog validation). Per the reference notes: "Every AI-derived attribute
/// carries an evidence excerpt + thread reference." The legacy evidence
/// shape ({excerpt, thread_ref}) is preserved; the numeric confidence maps
/// onto the operational vocabulary (> 0 → 'medium', else 'unknown').
/// Returns the inserted row id.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the write fails.
pub fn set_attribute(
    conn: &mut Connection,
    conversation_id: i64,
    key: AiAttributeKey,
    value: &str,
    evidence_excerpt: &str,
    thread_ref: &str,
    confidence: f64,
) -> Result<i64> {
    let record = crate::ai_attributes::AttributeRecord {
        key,
        value: value.to_string(),
        confidence: if confidence > 0.0 {
            "medium"
        } else {
            "unknown"
        },
        source: "ai",
        evidence: vec![serde_json::json!({
            "excerpt": evidence_excerpt,
            "thread_ref": thread_ref,
        })],
        run_id: None,
    };
    crate::ai_attributes::upsert_single(conn, conversation_id, record)
}

/// Current AI attribute rows for a conversation, mapped back onto the
/// analysis `AiAttribute` shape (first evidence entry wins).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn get_attributes(conn: &Connection, conversation_id: i64) -> Result<Vec<AiAttribute>> {
    let rows = crate::ai_attributes::current_for_conversation(conn, conversation_id)?;
    Ok(rows.into_iter().filter_map(row_to_ai_attribute).collect())
}

/// The current value of one attribute for a conversation.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn get_attribute(
    conn: &Connection,
    conversation_id: i64,
    key: AiAttributeKey,
) -> Result<Option<AiAttribute>> {
    let rows = crate::ai_attributes::current_for_conversation(conn, conversation_id)?;
    Ok(rows
        .into_iter()
        .find(|r| r.attribute == key.as_str())
        .and_then(row_to_ai_attribute))
}

/// Map one stored attribute row onto the analysis shape.
fn row_to_ai_attribute(row: crate::ai_attributes::AttributeRow) -> Option<AiAttribute> {
    let key = AiAttributeKey::parse(&row.attribute)?;
    let (evidence_excerpt, thread_ref) = row
        .evidence
        .as_array()
        .and_then(|a| a.first())
        .map(|e| {
            let excerpt = e.get("excerpt").and_then(|v| v.as_str()).unwrap_or("");
            let thread_ref = e
                .get("thread_ref")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    e.get("thread_local_id")
                        .and_then(|v| v.as_i64())
                        .map(|id| id.to_string())
                });
            (excerpt.to_string(), thread_ref.unwrap_or_default())
        })
        .unwrap_or_default();
    let confidence = match row.confidence.as_str() {
        "high" => 0.9,
        "medium" => 0.6,
        "low" => 0.3,
        _ => 0.0,
    };
    Some(AiAttribute {
        id: Some(row.id),
        conversation_id: row.conversation_id,
        key,
        value: row.value,
        evidence_excerpt,
        thread_ref,
        confidence,
        created_at: row.computed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_provider::FakeAiProvider;
    use crate::embeddings::apply_m008;
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
        apply_m008(&conn).unwrap();
        apply_m010(&conn).unwrap();
        crate::ai_attributes::apply_m033(&conn).unwrap();
        conn
    }

    fn insert_conversation(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id)
             VALUES (?1, ?1, ?1, 'T', 1, 1)",
            params![id],
        )
        .unwrap();
    }

    // ---- M010 migration ----------------------------------------------------

    #[test]
    fn m010_creates_ai_attributes_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_attributes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m033_replaces_the_m010_index_with_the_reference_indexes() {
        let conn = fresh_db();
        for idx in [
            "idx_ai_attributes_current",
            "idx_ai_attributes_value",
            "idx_ai_attributes_run",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing {idx}");
        }
    }

    #[test]
    fn m010_is_idempotent() {
        let conn = fresh_db();
        // After M033 the M010 apply is a deliberate no-op — re-running the
        // boot sequence must not fail on the rebuilt table.
        apply_m010(&conn).unwrap();
        apply_m010(&conn).unwrap();
    }

    // ---- set_attribute + get_attributes ------------------------------------

    #[test]
    fn set_attribute_stores_with_evidence_and_thread_ref() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);
        let id = set_attribute(
            &mut conn,
            1001,
            AiAttributeKey::Intent,
            "billing",
            "I want my money back",
            "msg_1",
            0.9,
        )
        .unwrap();
        assert!(id > 0);

        let (key, value, evidence, confidence, source): (String, String, String, String, String) =
            conn.query_row(
                "SELECT attribute, value, evidence, confidence, source
                 FROM ai_attributes WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(key, "intent");
        assert_eq!(value, "billing");
        let parsed: serde_json::Value = serde_json::from_str(&evidence).unwrap();
        assert_eq!(parsed[0]["excerpt"], "I want my money back");
        assert_eq!(parsed[0]["thread_ref"], "msg_1");
        assert_eq!(confidence, "medium", "confidence > 0 maps to 'medium'");
        assert_eq!(source, "ai");
    }

    #[test]
    fn get_attributes_returns_all_for_conversation() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);
        insert_conversation(&conn, 1002);
        set_attribute(
            &mut conn,
            1001,
            AiAttributeKey::Intent,
            "billing",
            "evidence1",
            "msg_1",
            0.9,
        )
        .unwrap();
        set_attribute(
            &mut conn,
            1001,
            AiAttributeKey::Urgency,
            "high",
            "evidence2",
            "msg_2",
            0.8,
        )
        .unwrap();
        set_attribute(
            &mut conn,
            1002,
            AiAttributeKey::Intent,
            "question",
            "evidence3",
            "msg_1",
            0.7,
        )
        .unwrap();

        let attrs = get_attributes(&conn, 1001).unwrap();
        assert_eq!(attrs.len(), 2, "only conv 1001's attributes");
        for a in &attrs {
            assert_eq!(a.conversation_id, 1001);
        }
    }

    #[test]
    fn get_attributes_for_empty_conversation_returns_empty() {
        let conn = fresh_db();
        let attrs = get_attributes(&conn, 9999).unwrap();
        assert!(attrs.is_empty());
    }

    #[test]
    fn get_attribute_returns_latest() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);
        // Insert two values for the same key — get_attribute should return the
        // latest (the versioned repo retires the previous one).
        set_attribute(
            &mut conn,
            1001,
            AiAttributeKey::Intent,
            "question",
            "old evidence",
            "msg_1",
            0.5,
        )
        .unwrap();
        set_attribute(
            &mut conn,
            1001,
            AiAttributeKey::Intent,
            "bug_report",
            "new evidence",
            "msg_2",
            0.9,
        )
        .unwrap();

        let attr = get_attribute(&conn, 1001, AiAttributeKey::Intent)
            .unwrap()
            .unwrap();
        assert_eq!(attr.value, "bug_report", "get_attribute returns the latest");
        assert_eq!(attr.evidence_excerpt, "new evidence");
    }

    #[test]
    fn get_attribute_returns_none_for_nonexistent() {
        let conn = fresh_db();
        let attr = get_attribute(&conn, 9999, AiAttributeKey::Intent).unwrap();
        assert!(attr.is_none());
    }

    // ---- all 14 keys round-trip --------------------------------------------

    #[test]
    fn all_14_attribute_keys_round_trip() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);
        for key in AiAttributeKey::ALL {
            // Enum keys need a value from their closed vocabulary; booleans
            // are 'true'/'false'; numbers must be finite.
            let value = match key.value_type() {
                crate::catalog::AttributeValueType::Enum => key.values()[0],
                crate::catalog::AttributeValueType::Boolean => "true",
                crate::catalog::AttributeValueType::Number => "3",
                _ => "test_value",
            };
            let id = set_attribute(
                &mut conn,
                1001,
                key,
                value,
                "test_evidence",
                "test_thread",
                0.5,
            )
            .unwrap();
            assert!(id > 0, "failed to insert {key:?}");
        }

        let attrs = get_attributes(&conn, 1001).unwrap();
        assert_eq!(attrs.len(), 14);
        for key in AiAttributeKey::ALL {
            assert!(
                attrs.iter().any(|a| a.key == key),
                "missing {key:?} in results"
            );
        }
    }

    // ---- parse_analysis_response -------------------------------------------

    #[test]
    fn parse_analysis_response_valid_json() {
        let json = r#"{"attributes": [
            {"key": "intent", "value": "refund", "evidence": "I want my money back", "thread_ref": "msg_1", "confidence": 0.9},
            {"key": "urgency", "value": "high", "evidence": "this is urgent", "thread_ref": "msg_2"}
        ]}"#;
        let attrs = parse_analysis_response(json, 1001);
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].key, AiAttributeKey::Intent);
        assert_eq!(attrs[0].value, "refund");
        assert_eq!(attrs[0].evidence_excerpt, "I want my money back");
        assert!((attrs[0].confidence - 0.9).abs() < 1e-6);
        assert_eq!(attrs[1].key, AiAttributeKey::Urgency);
        assert!(
            (attrs[1].confidence - 0.0).abs() < 1e-6,
            "missing confidence defaults to 0.0"
        );
    }

    #[test]
    fn parse_analysis_response_unknown_keys_are_filtered() {
        let json = r#"{"attributes": [
            {"key": "intent", "value": "refund", "evidence": "e", "thread_ref": "m1"},
            {"key": "not_a_real_key", "value": "x", "evidence": "e", "thread_ref": "m2"}
        ]}"#;
        let attrs = parse_analysis_response(json, 1001);
        assert_eq!(attrs.len(), 1, "unknown keys are filtered");
        assert_eq!(attrs[0].key, AiAttributeKey::Intent);
    }

    #[test]
    fn parse_analysis_response_malformed_json_returns_empty() {
        let attrs = parse_analysis_response("not valid json", 1001);
        assert!(
            attrs.is_empty(),
            "malformed AI output → empty (never crash)"
        );
    }

    #[test]
    fn parse_analysis_response_empty_attributes_returns_empty() {
        let json = r#"{"attributes": []}"#;
        let attrs = parse_analysis_response(json, 1001);
        assert!(attrs.is_empty());
    }

    // ---- analyze_conversation with FakeAiProvider --------------------------

    #[tokio::test]
    async fn analyze_conversation_with_fake_provider_returns_result() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new();

        // The Fake provider returns a canned response — not valid JSON.
        // So parse_analysis_response will return empty attributes.
        let result = analyze_conversation(
            &conn,
            &provider,
            "fake-chat-model",
            "Hello I need help",
            1001,
        )
        .await
        .unwrap();
        assert_eq!(result.conversation_id, 1001);
        assert!(!result.raw_response.is_empty());
        assert!(!result.from_cache, "first call is not cached");
        // The Fake provider's response isn't structured JSON, so attributes are empty.
        assert!(
            result.attributes.is_empty(),
            "Fake provider doesn't produce structured attributes"
        );
    }

    #[tokio::test]
    async fn analyze_conversation_caches_result() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new();

        // First call — cache miss.
        let r1 = analyze_conversation(&conn, &provider, "m", "same text", 1001)
            .await
            .unwrap();
        assert!(!r1.from_cache);

        // Second call — cache hit (same input → cached result).
        let r2 = analyze_conversation(&conn, &provider, "m", "same text", 1001)
            .await
            .unwrap();
        assert!(r2.from_cache, "second call is cached");
        assert_eq!(r1.raw_response, r2.raw_response, "cached response matches");
    }

    #[tokio::test]
    async fn analyze_conversation_different_text_not_cached() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new();

        let r1 = analyze_conversation(&conn, &provider, "m", "text A", 1001)
            .await
            .unwrap();
        let r2 = analyze_conversation(&conn, &provider, "m", "text B", 1001)
            .await
            .unwrap();
        assert!(!r1.from_cache);
        assert!(!r2.from_cache, "different text → cache miss");
    }

    // ---- AiAttribute serde -------------------------------------------------

    #[test]
    fn ai_attribute_serializes() {
        let attr = AiAttribute {
            id: Some(1),
            conversation_id: 1001,
            key: AiAttributeKey::Intent,
            value: "refund".into(),
            evidence_excerpt: "I want my money back".into(),
            thread_ref: "msg_1".into(),
            confidence: 0.9,
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        let s = serde_json::to_string(&attr).unwrap();
        assert!(s.contains("\"key\":\"intent\""));
        assert!(s.contains("\"value\":\"refund\""));
        assert!(s.contains("\"evidence_excerpt\":\"I want my money back\""));
    }

    #[test]
    fn analysis_result_serializes() {
        let result = AnalysisResult {
            conversation_id: 1001,
            attributes: vec![],
            raw_response: "test".into(),
            from_cache: false,
        };
        let s = serde_json::to_string(&result).unwrap();
        assert!(s.contains("\"conversation_id\":1001"));
        assert!(s.contains("\"from_cache\":false"));
    }

    // ---- Prompt redaction (spec #127) --------------------------------------

    /// The Fake provider echoes the user prompt back, so the response proves
    /// what the provider actually received: payment data must never reach an
    /// AI prompt (reference lmStudioProvider.chatJson `redact: true`).
    #[tokio::test]
    async fn analysis_prompt_is_redacted_before_the_provider_sees_it() {
        let conn = fresh_db();
        insert_conversation(&conn, 7);
        let provider = FakeAiProvider::new();
        let result = analyze_conversation(
            &conn,
            &provider,
            "fake-chat-model",
            "My card is 4111 1111 1111 1111 and my cvv: 1234 thanks",
            7,
        )
        .await
        .unwrap();
        assert!(
            result.raw_response.contains("[REDACTED-CARD]"),
            "card number must not reach the provider: {}",
            result.raw_response
        );
        assert!(
            result.raw_response.contains("[REDACTED-CVV]"),
            "cvv must not reach the provider: {}",
            result.raw_response
        );
        assert!(
            !result.raw_response.contains("4111 1111 1111 1111"),
            "raw card number must not appear: {}",
            result.raw_response
        );
    }

    /// The setting `redaction_enabled=false` disables the pass, exactly like
    /// the reference's `redactionEnabled` read.
    #[tokio::test]
    async fn analysis_prompt_redaction_can_be_disabled_by_setting() {
        let conn = fresh_db();
        insert_conversation(&conn, 7);
        crate::settings::set_bool(&conn, "redaction_enabled", false).unwrap();
        let provider = FakeAiProvider::new();
        let result = analyze_conversation(
            &conn,
            &provider,
            "fake-chat-model",
            "My card is 4111 1111 1111 1111 thanks",
            7,
        )
        .await
        .unwrap();
        assert!(
            result.raw_response.contains("4111 1111 1111 1111"),
            "with redaction off the provider sees the raw text: {}",
            result.raw_response
        );
    }
}
