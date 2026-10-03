//! Copilot routes — mirrors src/server/routes/copilot.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/copilot/sessions
pub async fn list_sessions(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let sessions: Vec<Value> = conn
        .prepare("SELECT id, conversation_id, created_at FROM ai_runs WHERE type = 'copilot' ORDER BY created_at DESC LIMIT 50")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "conversation_id": r.get::<_, Option<i64>>(1)?,
                    "created_at": r.get::<_, String>(2)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"sessions": sessions}))
}

/// GET /api/copilot/sessions/:id
pub async fn get_session(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT id, conversation_id, created_at, result_json FROM ai_runs WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "conversation_id": r.get::<_, Option<i64>>(1)?,
                "created_at": r.get::<_, String>(2)?,
                "messages": r.get::<_, Option<String>>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Session not found"})),
    }
}

/// POST /api/copilot/chat
pub async fn chat(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let conversation_id = body.get("conversationId").and_then(|v| v.as_i64());
    Json(json!({
        "response": "Copilot is not available without a configured AI provider.",
        "message": message,
        "conversationId": conversation_id,
        "tools_used": [],
    }))
}

/// DELETE /api/copilot/sessions/:id
pub async fn delete_session(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute("DELETE FROM ai_runs WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}

/// GET /api/copilot/tools
pub async fn tools(State(_state): State<AppState>) -> Json<Value> {
    use crate::catalog::CopilotTool;
    let tools: Vec<Value> = CopilotTool::ALL
        .iter()
        .map(|t| {
            json!({
                "id": t.as_str(),
                "description": t.description(),
            })
        })
        .collect();
    Json(json!({
        "tools": tools,
        "note": "Copilot tools available — actual AI responses require a configured AI provider.",
    }))
}

/// GET /api/copilot/starter-questions/:conversationId — deterministic starter
/// questions (reference copilot.ts:79 → ai/copilot.ts starterQuestions()).
///
/// The question set is derived from conversation facts (prior tickets, known
/// issue links, analyses, attributes); 0 questions = 404 (not found).
pub async fn starter_questions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    let Some(conv_id) =
        crate::conversation_ops::js_number(&id).filter(|v| v.fract() == 0.0 && *v > 0.0)
    else {
        return Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": "Conversation id must be a positive integer."
        }));
    };
    let conv_id = conv_id as i64;
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let facts: Option<(i64, Option<i64>, i64, i64, i64)> = conn
        .query_row(
            "SELECT c.id, c.customer_local_id,
                (SELECT COUNT(*) FROM conversations c2
                  WHERE c2.customer_local_id = c.customer_local_id
                    AND c2.deleted_at IS NULL AND c2.id != c.id),
                (SELECT COUNT(*) FROM known_issue_conversations kic WHERE kic.conversation_id = c.id),
                (SELECT COUNT(*) FROM ai_runs ar
                  WHERE ar.conversation_id = c.id AND ar.type = 'ticket_analysis' AND ar.status = 'completed')
             FROM conversations c WHERE c.id = ?1 AND c.deleted_at IS NULL",
            rusqlite::params![conv_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    // attributes count: current rows only (superseded_at IS NULL)
                    0,
                ))
            },
        )
        .ok();
    let Some((_, _, prior_tickets, known_issue_links, analyses)) = facts else {
        return Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": "Conversation not found."
        }));
    };
    let attributes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_attributes WHERE conversation_id = ?1 AND superseded_at IS NULL",
            rusqlite::params![conv_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let mut questions: Vec<(String, String)> = vec![
        (
            "What is this customer asking?".into(),
            "Summarizes the current conversation".into(),
        ),
        (
            "What should I check before replying?".into(),
            "Pre-reply checklist from local evidence".into(),
        ),
        (
            "What information has already been provided?".into(),
            "Avoids asking the customer twice".into(),
        ),
        (
            "Why is this ticket currently considered urgent?".into(),
            "Explains urgency from AI attributes + analysis".into(),
        ),
    ];
    if prior_tickets > 0 {
        questions.push((
            "What happened in their previous tickets?".into(),
            format!("{prior_tickets} previous ticket(s) in the local archive"),
        ));
        questions.push((
            "What changed since the last interaction?".into(),
            "Compares with the last conversation".into(),
        ));
        questions.push((
            "Summarize the last three conversations.".into(),
            "Recent history digest".into(),
        ));
    }
    questions.push((
        "Have we seen this issue before?".into(),
        "Searches similar conversations".into(),
    ));
    questions.push((
        "What solved the previous cases?".into(),
        "Resolutions from similar tickets".into(),
    ));
    questions.push((
        "Has another customer had the same problem?".into(),
        "Cross-customer search".into(),
    ));
    if known_issue_links > 0 {
        questions.push((
            "Which known issue is this linked to?".into(),
            "Known-issue linkage and status".into(),
        ));
    }
    if analyses > 0 || attributes > 0 {
        questions.push((
            "What does the AI analysis say?".into(),
            "Summarizes the stored AI analysis and attributes".into(),
        ));
    }
    Json(json!({
        "questions": questions
            .iter()
            .map(|(q, why)| json!({ "question": q, "why": why }))
            .collect::<Vec<_>>()
    }))
}
