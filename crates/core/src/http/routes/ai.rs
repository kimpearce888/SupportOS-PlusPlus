//! AI routes — mirrors src/server/routes/ai.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/ai/status
///
/// Reference response shape:
/// ```json
/// {
///   "ai_enabled": true,
///   "settings": { "base_url": "...", "chat_model": "...", "embedding_model": "...", "timeout_ms": N, "concurrency": N },
///   "lmstudio": { "connected": false, "models": [], "error": null },
///   "last_inference": null,
///   "provider": "lmstudio",
///   "queued_ai_jobs": 0,
///   "failed_ai_jobs": 0,
///   "index": { "conversations_indexed": N, "chunks_indexed": N, "chunks_pending": N, "chunks_failed": N }
/// }
/// ```
pub async fn status(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Get AI provider info.
    let provider_kind = match crate::ai_center::get_ai_status(&conn) {
        Ok(s) => match s.provider_kind {
            crate::ai_center::ProviderKind::LmStudio => "lmstudio",
            crate::ai_center::ProviderKind::Ollama => "ollama",
            crate::ai_center::ProviderKind::Generic => "generic",
            crate::ai_center::ProviderKind::None => "none",
        }
        .to_string(),
        Err(_) => "none".to_string(),
    };
    let chat_model = crate::settings::get_string(&conn, "ai_chat_model")
        .ok()
        .flatten()
        .unwrap_or_default();
    let embedding_model = crate::settings::get_string(&conn, "ai_embedding_model")
        .ok()
        .flatten()
        .unwrap_or_default();
    let base_url = crate::settings::get_string(&conn, "ai_base_url")
        .ok()
        .flatten()
        .unwrap_or_default();
    let ai_enabled = crate::settings::get_bool(&conn, "ai_enabled", true).unwrap_or(true);
    let last_inference: Option<String> = conn
        .query_row("SELECT MAX(created_at) FROM ai_runs", [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten();
    let queued_ai_jobs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind LIKE 'ai%' AND state = 'queued'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let failed_ai_jobs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind LIKE 'ai%' AND state = 'failed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let conversations_indexed: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations_fts", [], |r| r.get(0))
        .unwrap_or(0);
    let chunks_indexed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_runs WHERE type = 'embedding'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({
        "ai_enabled": ai_enabled,
        "settings": {
            "base_url": base_url,
            "chat_model": chat_model,
            "embedding_model": embedding_model,
            "timeout_ms": 30000,
            "concurrency": 2,
        },
        "lmstudio": {
            "connected": false,
            "models": [],
            "error": null,
        },
        "last_inference": last_inference,
        "provider": provider_kind,
        "queued_ai_jobs": queued_ai_jobs,
        "failed_ai_jobs": failed_ai_jobs,
        "index": {
            "conversations_indexed": conversations_indexed,
            "chunks_indexed": chunks_indexed,
            "chunks_pending": 0,
            "chunks_failed": 0,
        },
    }))
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
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
///
/// Reference response shape: `{ "jobs": [...] }`
pub async fn jobs(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let jobs: Vec<Value> = conn
        .prepare("SELECT id, kind, state, attempts, available_at, claimed_at, completed_at, last_error FROM jobs WHERE kind LIKE 'ai%' ORDER BY id DESC LIMIT 50")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "attempts": r.get::<_, i64>(3)?,
                    "created_at": r.get::<_, Option<String>>(4)?,
                    "claimed_at": r.get::<_, Option<String>>(5)?,
                    "completed_at": r.get::<_, Option<String>>(6)?,
                    "error": r.get::<_, Option<String>>(7)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"jobs": jobs}))
}

/// GET /api/ai/analytics — same shape as /api/analytics/ai.
pub async fn ai_analytics(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM ai_runs", [], |r| r.get(0))
        .unwrap_or(0);
    let successful: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_runs WHERE response_json IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let draft_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM verified_drafts", [], |r| r.get(0))
        .unwrap_or(0);
    let draft_accepted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM verified_drafts WHERE status = 'accepted'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let draft_rejected: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM verified_drafts WHERE status = 'rejected'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let analysis_success_rate = if total > 0 {
        (successful as f64) / (total as f64)
    } else {
        0.0
    };
    Json(json!({
        "tickets_analyzed": total,
        "analysis_success_rate": analysis_success_rate,
        "draft_count": draft_count,
        "draft_accepted": draft_accepted,
        "draft_rejected": draft_rejected,
        "draft_edit_rate": 0.0,
        "verification_warnings": 0,
        "unsupported_claim_rate": 0,
        "common_failure_patterns": [],
        "source": "local",
    }))
}

/// GET /api/ai/evaluation
pub async fn evaluation(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "evaluation_mode": "off",
        "tests": [],
        "mode": "off",
        "message": "AI evaluation mode is permanently OFF. No tests are run."
    }))
}
