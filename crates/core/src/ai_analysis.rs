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
//!
//! M6-T03 (attributes): M010 migration creates the `ai_attributes` table.
//! `set_attribute` stores a derived attribute with evidence + thread_ref.
//! `get_attributes` retrieves all attributes for a conversation.
//!
//! Per spec: AI attributes never overwrite Help Scout source data — they're a
//! separate layer, like `supportos_priority` from M3-T04. The analysis is cached
//! via the `ai_runs` table (M5-T07).

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

/// Apply M010 migration. Idempotent.
pub fn apply_m010(conn: &Connection) -> Result<()> {
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

/// Store a derived AI attribute. Per the reference notes: "Every AI-derived
/// attribute carries an evidence excerpt + thread reference."
///
/// # Errors
///
/// Returns `Error::Sqlite` if the insert fails.
pub fn set_attribute(
    conn: &Connection,
    conversation_id: i64,
    key: AiAttributeKey,
    value: &str,
    evidence_excerpt: &str,
    thread_ref: &str,
    confidence: f64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO ai_attributes
            (conversation_id, attribute_key, value, evidence_excerpt, thread_ref, confidence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            conversation_id,
            key.as_str(),
            value,
            evidence_excerpt,
            thread_ref,
            confidence
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all AI attributes for a conversation.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn get_attributes(conn: &Connection, conversation_id: i64) -> Result<Vec<AiAttribute>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, attribute_key, value, evidence_excerpt, thread_ref, confidence, created_at
         FROM ai_attributes
         WHERE conversation_id = ?1
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map(params![conversation_id], |r| {
            let key_str: String = r.get(2)?;
            let key = AiAttributeKey::ALL
                .iter()
                .find(|k| k.as_str() == key_str.as_str())
                .copied()
                .ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("unknown attribute key: {key_str}"),
                        )),
                    )
                })?;
            Ok(AiAttribute {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                key,
                value: r.get(3)?,
                evidence_excerpt: r.get(4)?,
                thread_ref: r.get(5)?,
                confidence: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Get a specific attribute for a conversation.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn get_attribute(
    conn: &Connection,
    conversation_id: i64,
    key: AiAttributeKey,
) -> Result<Option<AiAttribute>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, attribute_key, value, evidence_excerpt, thread_ref, confidence, created_at
         FROM ai_attributes
         WHERE conversation_id = ?1 AND attribute_key = ?2
         ORDER BY created_at DESC LIMIT 1",
    )?;
    let row: Option<AiAttribute> = stmt
        .query_row(params![conversation_id, key.as_str()], |r| {
            let key_str: String = r.get(2)?;
            let key = AiAttributeKey::ALL
                .iter()
                .find(|k| k.as_str() == key_str.as_str())
                .copied()
                .ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("unknown attribute key: {key_str}"),
                        )),
                    )
                })?;
            Ok(AiAttribute {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                key,
                value: r.get(3)?,
                evidence_excerpt: r.get(4)?,
                thread_ref: r.get(5)?,
                confidence: r.get(6)?,
                created_at: r.get(7)?,
            })
        })
        .ok();
    Ok(row)
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
        conn
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
    fn m010_creates_index() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_ai_attributes_conv'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn m010_is_idempotent() {
        let conn = fresh_db();
        apply_m010(&conn).unwrap();
    }

    // ---- set_attribute + get_attributes ------------------------------------

    #[test]
    fn set_attribute_stores_with_evidence_and_thread_ref() {
        let conn = fresh_db();
        let id = set_attribute(
            &conn,
            1001,
            AiAttributeKey::Intent,
            "refund request",
            "I want my money back",
            "msg_1",
            0.9,
        )
        .unwrap();
        assert!(id > 0);

        let (key, value, evidence, thread, conf): (String, String, String, String, f64) = conn
            .query_row(
                "SELECT attribute_key, value, evidence_excerpt, thread_ref, confidence
                 FROM ai_attributes WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(key, "intent");
        assert_eq!(value, "refund request");
        assert_eq!(evidence, "I want my money back");
        assert_eq!(thread, "msg_1");
        assert!((conf - 0.9).abs() < 1e-6);
    }

    #[test]
    fn get_attributes_returns_all_for_conversation() {
        let conn = fresh_db();
        set_attribute(
            &conn,
            1001,
            AiAttributeKey::Intent,
            "refund",
            "evidence1",
            "msg_1",
            0.9,
        )
        .unwrap();
        set_attribute(
            &conn,
            1001,
            AiAttributeKey::Urgency,
            "high",
            "evidence2",
            "msg_2",
            0.8,
        )
        .unwrap();
        set_attribute(
            &conn,
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
        let conn = fresh_db();
        // Insert two values for the same key — get_attribute should return the latest.
        set_attribute(
            &conn,
            1001,
            AiAttributeKey::Intent,
            "old",
            "old evidence",
            "msg_1",
            0.5,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        set_attribute(
            &conn,
            1001,
            AiAttributeKey::Intent,
            "new",
            "new evidence",
            "msg_2",
            0.9,
        )
        .unwrap();

        let attr = get_attribute(&conn, 1001, AiAttributeKey::Intent)
            .unwrap()
            .unwrap();
        assert_eq!(attr.value, "new", "get_attribute returns the latest");
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
        let conn = fresh_db();
        for key in AiAttributeKey::ALL {
            let id = set_attribute(
                &conn,
                1001,
                key,
                "test_value",
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
}
