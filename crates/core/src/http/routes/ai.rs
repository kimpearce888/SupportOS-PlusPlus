//! AI routes — mirrors src/server/routes/ai.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/ai/status
pub async fn status(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::ai_center::get_ai_status(&conn) {
        Ok(s) => Json(serde_json::to_value(&s).unwrap_or(json!({}))),
        Err(_) => Json(json!({"provider_kind": "none"})),
    }
}

/// POST /api/ai/analyze/:conversationId
pub async fn analyze(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    Json(
        json!({"analysis": null, "conversationId": conversation_id, "message": "AI analysis not available without a configured AI provider."}),
    )
}

/// POST /api/ai/draft/:conversationId
pub async fn draft(State(state): State<AppState>, Path(conversation_id): Path<i64>) -> Json<Value> {
    Json(
        json!({"draft": null, "conversationId": conversation_id, "message": "AI draft not available without a configured AI provider."}),
    )
}

/// POST /api/ai/draft/:draftId/rewrite
pub async fn rewrite(State(state): State<AppState>, Path(draft_id): Path<i64>) -> Json<Value> {
    Json(json!({"draft": null, "draftId": draft_id, "message": "AI rewrite not available."}))
}

/// POST /api/ai/draft/:draftId/verify
pub async fn verify(State(state): State<AppState>, Path(draft_id): Path<i64>) -> Json<Value> {
    Json(
        json!({"verified": false, "draftId": draft_id, "message": "AI verification not available."}),
    )
}

/// POST /api/ai/draft/:draftId/feedback
pub async fn feedback(State(state): State<AppState>, Path(draft_id): Path<i64>) -> Json<Value> {
    Json(json!({"ok": true, "draftId": draft_id}))
}

/// GET /api/ai/similar/:conversationId
pub async fn similar(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    Json(json!({"similar": [], "conversationId": conversation_id}))
}

/// GET /api/ai/memory/:customerId
pub async fn get_memory(
    State(state): State<AppState>,
    Path(customer_id): Path<i64>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let memories: Vec<Value> = conn
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
    Json(json!({"memories": memories, "customerId": customer_id}))
}

/// POST /api/ai/memory/:customerId
pub async fn set_memory(
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
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "INSERT INTO customer_memory (customer_id, key, value, source, created_at) VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![customer_id, key, value, source],
    );
    Json(json!({"ok": true, "customerId": customer_id}))
}

/// POST /api/ai/cluster-issues
pub async fn cluster_issues(State(state): State<AppState>) -> Json<Value> {
    Json(
        json!({"clusters": [], "message": "Issue clustering not available without a configured AI provider."}),
    )
}

/// GET /api/ai/jobs
pub async fn jobs(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let queued: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind LIKE 'ai%' AND status = 'queued'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let failed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind LIKE 'ai%' AND status = 'failed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({"queued": queued, "failed": failed}))
}

/// GET /api/ai/analytics
pub async fn ai_analytics(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM ai_runs", [], |r| r.get(0))
        .unwrap_or(0);
    Json(json!({"total_runs": total, "by_type": {}}))
}

/// GET /api/ai/evaluation
pub async fn evaluation(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"mode": "off", "message": "AI evaluation mode is permanently OFF."}))
}
