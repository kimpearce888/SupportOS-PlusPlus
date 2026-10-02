//! Coaching routes — mirrors src/server/routes/coaching.ts
//!
//! Per-agent coaching plans + review queue. Coaching entries are
//! AI-generated (when an AI provider is configured) or manually added.
//! Stored in the `coaching_plans` table.

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/coaching/meta — coaching program metadata.
pub async fn meta(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM coaching_plans", [], |r| r.get(0))
        .unwrap_or(0);
    let open: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM coaching_plans WHERE status = 'open'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let resolved: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM coaching_plans WHERE status = 'resolved'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({"total": total, "open": open, "resolved": resolved}))
}

/// GET /api/coaching/:userId — coaching plans for a specific user.
pub async fn get_coaching(State(state): State<AppState>, Path(user_id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let plans: Vec<Value> = conn
        .prepare("SELECT id, user_id, conversation_id, summary, status, created_at, resolved_at FROM coaching_plans WHERE user_id = ?1 ORDER BY created_at DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![user_id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "user_id": r.get::<_, i64>(1)?,
                    "conversation_id": r.get::<_, Option<i64>>(2)?,
                    "summary": r.get::<_, String>(3)?,
                    "status": r.get::<_, String>(4)?,
                    "created_at": r.get::<_, String>(5)?,
                    "resolved_at": r.get::<_, Option<String>>(6)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"userId": user_id, "plans": plans}))
}

/// POST /api/coaching/:planId/review — mark a coaching plan as reviewed.
pub async fn review(State(state): State<AppState>, Path(plan_id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let rows = conn
        .execute(
            "UPDATE coaching_plans SET status = 'reviewed', resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            rusqlite::params![plan_id],
        )
        .unwrap_or(0);
    drop(conn);
    if rows > 0 {
        crate::http::event_bus::notify_sync(&state.bus, "coaching", 1);
    }
    Json(json!({"ok": rows > 0, "planId": plan_id}))
}
