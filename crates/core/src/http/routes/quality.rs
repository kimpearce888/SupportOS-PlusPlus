//! Quality routes — mirrors src/server/routes/quality.ts
//!
//! Quality Assurance (QA), Friction Scores, and Knowledge Gaps.
//! These aggregate metrics over conversations to surface trends, common
//! failure modes, and docs that need a refresh.

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/knowledge/gaps — knowledge gap report.
pub async fn list_gaps(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Aggregate gaps from the knowledge_gaps table (created by intelligence_features).
    let totals = json!({
        "open": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE status = 'open'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
        "resolved": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE status = 'resolved'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
    });
    let kinds = json!({
        "missing_topic": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE kind = 'missing_topic'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
        "outdated_doc": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE kind = 'outdated_doc'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
        "ambiguous_answer": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE kind = 'ambiguous_answer'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
    });
    Json(json!({
        "totals": totals,
        "kinds": kinds,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "notes": "Knowledge gaps aggregated from the local knowledge_gaps table."
    }))
}

/// POST /api/knowledge/gaps/rebuild
pub async fn rebuild_gaps(State(state): State<AppState>) -> Json<Value> {
    crate::http::event_bus::notify_sync(&state.bus, "knowledge_gaps", 1);
    Json(json!({"ok": true, "message": "Knowledge gap rebuild queued."}))
}

/// GET /api/qa/overview — QA report overview.
pub async fn qa_overview(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let closed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE status = 'closed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // with_ai_layer: conversations that have a post_resolution_qa row.
    let with_ai: i64 = conn
        .query_row("SELECT COUNT(*) FROM post_resolution_qa", [], |r| r.get(0))
        .unwrap_or(0);
    let qa_rows: Vec<Value> = conn
        .prepare("SELECT conversation_id, score, summary, created_at FROM post_resolution_qa ORDER BY created_at DESC LIMIT 100")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "conversation_id": r.get::<_, i64>(0)?,
                    "score": r.get::<_, Option<f64>>(1)?,
                    "summary": r.get::<_, Option<String>>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "closed_conversations": closed,
        "with_ai_layer": with_ai,
        "qa_rows": qa_rows,
        "generated_at": chrono::Utc::now().to_rfc3339(),
    }))
}

/// POST /api/qa/rebuild
pub async fn qa_rebuild(State(state): State<AppState>) -> Json<Value> {
    crate::http::event_bus::notify_sync(&state.bus, "qa", 1);
    Json(json!({"ok": true, "message": "QA rebuild queued."}))
}

/// GET /api/qa/:conversationId
pub async fn qa_conversation(
    State(state): State<AppState>,
    Path(conv_id): Path<i64>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT conversation_id, score, summary, created_at FROM post_resolution_qa WHERE conversation_id = ?1",
        rusqlite::params![conv_id],
        |r| {
            Ok(json!({
                "conversation_id": r.get::<_, i64>(0)?,
                "score": r.get::<_, Option<f64>>(1)?,
                "summary": r.get::<_, Option<String>>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"conversation_id": conv_id, "score": null, "summary": null})),
    }
}

/// POST /api/qa/:conversationId/analyze
pub async fn qa_analyze(State(state): State<AppState>, Path(conv_id): Path<i64>) -> Json<Value> {
    crate::http::event_bus::notify_sync(&state.bus, "qa", 1);
    Json(json!({"ok": true, "conversation_id": conv_id, "message": "QA analysis queued."}))
}

/// GET /api/friction/overview — friction score report.
pub async fn friction_overview(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Aggregate friction scores by kind + customer.
    let kinds: Vec<Value> = conn
        .prepare("SELECT kind, COUNT(*), AVG(score) FROM friction_scores GROUP BY kind ORDER BY 2 DESC LIMIT 10")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "kind": r.get::<_, String>(0)?,
                    "count": r.get::<_, i64>(1)?,
                    "avg_score": r.get::<_, Option<f64>>(2)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let customers: Vec<Value> = conn
        .prepare("SELECT customer_id, AVG(score) FROM friction_scores GROUP BY customer_id ORDER BY 2 DESC LIMIT 10")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "customer_id": r.get::<_, i64>(0)?,
                    "avg_score": r.get::<_, Option<f64>>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "days": 30,
        "kinds": kinds,
        "customers_most_affected": customers,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "notes": "Friction overview aggregated from the local friction_scores table."
    }))
}

/// POST /api/friction/rebuild
pub async fn friction_rebuild(State(state): State<AppState>) -> Json<Value> {
    crate::http::event_bus::notify_sync(&state.bus, "friction", 1);
    Json(json!({"ok": true, "message": "Friction rebuild queued."}))
}
