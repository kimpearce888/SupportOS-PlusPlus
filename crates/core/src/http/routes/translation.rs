//! Translation routes — mirrors src/server/routes/translation.ts
//!
//! In the reference, translation delegates to a translation provider (DeepL etc).
//! The port does not bundle a translation provider, so these endpoints return
//! deterministic stubs that match the reference's API shape. A real translation
//! provider can be plugged in via the AI provider trait.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/translation/meta — list supported languages + provider info.
pub async fn meta(State(_state): State<AppState>) -> Json<Value> {
    // Static list of common languages (BCP-47 codes). Matches the
    // reference's fallback when no translation provider is configured.
    let languages = json!([
        {"code": "en", "name": "English"},
        {"code": "es", "name": "Spanish"},
        {"code": "fr", "name": "French"},
        {"code": "de", "name": "German"},
        {"code": "it", "name": "Italian"},
        {"code": "pt", "name": "Portuguese"},
        {"code": "nl", "name": "Dutch"},
        {"code": "ja", "name": "Japanese"},
        {"code": "ko", "name": "Korean"},
        {"code": "zh", "name": "Chinese"},
        {"code": "ar", "name": "Arabic"},
        {"code": "hi", "name": "Hindi"},
        {"code": "ru", "name": "Russian"}
    ]);
    Json(json!({
        "provider": "none",
        "languages": languages,
        "agent_language": "en",
        "note": "No translation provider configured. Text is returned unchanged."
    }))
}

/// POST /api/translation/detect — detect the language of a text.
pub async fn detect(State(_state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    // Naive heuristic: ASCII-only text defaults to English; CJK characters
    // map to their language; otherwise unknown. Real detection needs a
    // provider, but this is enough for the UI's "show original" toggle.
    let detected = if text.is_empty() {
        "unknown"
    } else if text.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)) {
        "zh"
    } else if text.chars().any(|c| ('\u{3040}'..='\u{309F}').contains(&c)) {
        "ja"
    } else if text.chars().any(|c| ('\u{AC00}'..='\u{D7AF}').contains(&c)) {
        "ko"
    } else if text.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)) {
        "ar"
    } else if text.is_ascii() {
        "en"
    } else {
        "unknown"
    };
    Json(json!({"language": detected, "confidence": 0.5}))
}

/// GET /api/translation/conversation/:id — list translations for a conversation.
pub async fn conversation(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let translations: Vec<Value> = conn
        .prepare("SELECT id, conversation_id, thread_id, target_language, translated_text, created_at FROM conversation_translations WHERE conversation_id = ?1 ORDER BY created_at DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "conversation_id": r.get::<_, i64>(1)?,
                    "thread_id": r.get::<_, Option<i64>>(2)?,
                    "target_language": r.get::<_, String>(3)?,
                    "translated_text": r.get::<_, String>(4)?,
                    "created_at": r.get::<_, String>(5)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"conversationId": id, "translations": translations}))
}

/// POST /api/translation/translate — translate a text (no-op without a provider).
pub async fn translate(State(_state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let target = body.get("target").and_then(|v| v.as_str()).unwrap_or("en");
    // Without a translation provider, return the text unchanged.
    Json(json!({
        "translated_text": text,
        "target_language": target,
        "provider": "none",
        "message": "No translation provider configured. Text returned unchanged."
    }))
}
