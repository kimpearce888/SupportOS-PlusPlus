//! Memory routes — mirrors src/server/routes/memory.ts
//!
//! Customer memory entries — AI-generated + manually added notes about a
//! customer (preferences, history, signal-overrides). Stored in the
//! `customer_memory` table.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/memory/meta — overall memory store stats.
pub async fn meta(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM customer_memory", [], |r| r.get(0))
        .unwrap_or(0);
    let by_source: Vec<Value> = conn
        .prepare("SELECT source, COUNT(*) FROM customer_memory GROUP BY source ORDER BY 2 DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "source": r.get::<_, String>(0)?,
                    "count": r.get::<_, i64>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "total": total,
        "by_source": by_source,
        "sections": by_source.iter().map(|s| s["source"].clone()).collect::<Vec<_>>(),
        "section_labels": by_source.iter().map(|s| {
            let src = s["source"].as_str().unwrap_or("manual");
            let label = match src {
                "ai" => "AI-generated",
                "manual" => "Manually added",
                "imported" => "Imported",
                _ => src,
            };
            json!({src: label})
        }).collect::<Vec<_>>(),
        "entry_kinds": ["preference", "history", "signal", "note"],
        "notes": "Memory stats aggregated from the local customer_memory table."
    }))
}

/// GET /api/memory/:customerId — list memory entries for a customer.
pub async fn get(State(state): State<AppState>, Path(customer_id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let entries: Vec<Value> = conn
        .prepare("SELECT id, customer_id, key, value, source, created_at FROM customer_memory WHERE customer_id = ?1 ORDER BY created_at DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![customer_id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "customer_id": r.get::<_, i64>(1)?,
                    "key": r.get::<_, String>(2)?,
                    "value": r.get::<_, String>(3)?,
                    "source": r.get::<_, String>(4)?,
                    "created_at": r.get::<_, String>(5)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"entries": entries, "customerId": customer_id}))
}

/// POST /api/memory/:customerId/entries — add a memory entry.
pub async fn add_entry(
    State(state): State<AppState>,
    Path(customer_id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("note");
    let value = body.get("value").and_then(|v| v.as_str()).unwrap_or("");
    let source = body
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("manual");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let inserted = conn
        .execute(
            "INSERT INTO customer_memory (customer_id, key, value, source, created_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params![customer_id, key, value, source],
        )
        .is_ok();
    drop(conn);
    if inserted {}
    Json(json!({"ok": inserted, "customerId": customer_id}))
}

// ─── Entry deletion (reference memory.ts:78-100 + shared/memory.ts) ──────

/// Red-line patterns with BOTH word boundaries, case-insensitive
/// (reference MEMORY_QUARANTINE_PATTERNS: personality, temperament,
/// mental health, emotional state/instability, cognitive trait,
/// mood/anxiety disorder, diagnos(is|ed|es), depress(ed|ion), bipolar,
/// intelligence level/score).
const QUARANTINE_WORDS: [&str; 16] = [
    "personality",
    "temperament",
    "mental health",
    "emotional state",
    "emotional instability",
    "cognitive trait",
    "mood disorder",
    "anxiety disorder",
    "bipolar",
    "diagnosis",
    "diagnosed",
    "diagnoses",
    "depressed",
    "depression",
    "intelligence level",
    "intelligence score",
];

/// Red-line patterns with only a LEADING word boundary (reference
/// `/\bpsycholog/i`, `/\bintrovert/i`, ... have no trailing `\b`, so
/// "psychology", "introverted", "narcissistic", "schizophrenia" and
/// "autism"/"autistic" all match).
const QUARANTINE_PREFIXES: [&str; 8] = [
    "psycholog",
    "introvert",
    "extrovert",
    "extravert",
    "neurotic",
    "narcissis",
    "schizo",
    "autis",
];

/// JS `\b` word characters are exactly `[A-Za-z0-9_]`.
fn is_js_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Case-insensitive `\b<needle>\b` on a pre-lowercased haystack.
fn has_lower_word(hay: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(idx) = hay[start..].find(needle) {
        let at = start + idx;
        let before_ok = !hay[..at].chars().next_back().is_some_and(is_js_word_char);
        let after = at + needle.len();
        let after_ok = !hay[after..].chars().next().is_some_and(is_js_word_char);
        if before_ok && after_ok {
            return true;
        }
        start = at + needle.len();
    }
    false
}

/// Case-insensitive `\b<needle>` (leading boundary only).
fn has_lower_prefix(hay: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(idx) = hay[start..].find(needle) {
        let at = start + idx;
        if !hay[..at].chars().next_back().is_some_and(is_js_word_char) {
            return true;
        }
        start = at + needle.len();
    }
    false
}

/// Case-SENSITIVE `\bIQ\b` / `\bEQ\b` (the reference regexes carry no `i`).
fn has_case_word(text: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(idx) = text[start..].find(word) {
        let at = start + idx;
        let before_ok = !text[..at].chars().next_back().is_some_and(is_js_word_char);
        let after = at + word.len();
        let after_ok = !text[after..].chars().next().is_some_and(is_js_word_char);
        if before_ok && after_ok {
            return true;
        }
        start = at + word.len();
    }
    false
}

/// Reference shared/memory.ts isQuarantined: every red-line pattern is
/// tested against BOTH the key and `"{key} {value}"` (the key alone when
/// the value is null).
pub fn is_quarantined(key: &str, value: Option<&str>) -> bool {
    let text = value.map_or(key.to_string(), |v| format!("{key} {v}"));
    let key_lower = key.to_lowercase();
    let text_lower = text.to_lowercase();
    for w in QUARANTINE_WORDS {
        if has_lower_word(&key_lower, w) || has_lower_word(&text_lower, w) {
            return true;
        }
    }
    for p in QUARANTINE_PREFIXES {
        if has_lower_prefix(&key_lower, p) || has_lower_prefix(&text_lower, p) {
            return true;
        }
    }
    has_case_word(key, "IQ") || has_case_word(&text, "IQ") || has_case_word(&text, "EQ")
}

/// DELETE /api/memory/:customerId/entries/:entryId — delete a memory row.
/// Human rows are always deletable; AI rows are immutable through this path
/// EXCEPT quarantined ones (red-line entries can always be purged by a
/// human) — reference memory.ts:78-100 + customerMemoryService.deleteEntry.
pub async fn delete_entry(
    State(state): State<AppState>,
    Path((customer_id, entry_id)): Path<(String, String)>,
) -> Response {
    // Reference: both ids must be positive integers else 422.
    let (Ok(customer_id), Ok(entry_id)) = (customer_id.parse::<i64>(), entry_id.parse::<i64>())
    else {
        return memory_ids_422();
    };
    if customer_id <= 0 || entry_id <= 0 {
        return memory_ids_422();
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT memory_key, memory_value FROM customer_memory
             WHERE id = ?1 AND customer_id = ?2",
            rusqlite::params![entry_id, customer_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((key, value)) = row else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Memory entry not found for this customer."
            })),
        )
            .into_response();
    };
    // AI-extracted rows are immutable unless quarantined. Rows written by
    // the (M035) `source` column default to 'ai'.
    let source: String = conn
        .query_row(
            "SELECT COALESCE(source, 'ai') FROM customer_memory WHERE id = ?1",
            rusqlite::params![entry_id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "ai".into());
    if source != "human" && !is_quarantined(&key, value.as_deref()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "statusCode": 403,
                "error": "Forbidden",
                "message": "AI-extracted memories are immutable by design (they re-derive from conversations). Only human-written entries - and quarantined entries - can be deleted."
            })),
        )
            .into_response();
    }
    let _ = conn.execute(
        "DELETE FROM customer_memory WHERE id = ?1 AND customer_id = ?2",
        rusqlite::params![entry_id, customer_id],
    );
    (StatusCode::OK, Json(json!({"ok": true}))).into_response()
}

fn memory_ids_422() -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": "Customer id and entry id must be positive integers."
        })),
    )
        .into_response()
}
