//! Issues routes — mirrors src/server/routes/issues.ts

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/issues/clusters
pub async fn list_clusters(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let clusters: Vec<Value> = conn
        .prepare("SELECT id, name, status, created_at FROM issue_clusters ORDER BY id DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"clusters": clusters}))
}

/// GET /api/issues/sla-alerts
///
/// Reference response shape:
/// ```json
/// {
///   "generated_at": "...",
///   "total_breached": N, "total_at_risk": N,
///   "alerts": [...],
///   "per_mailbox": [],
///   "unconfigured_mailboxes": [],
///   "note": "..."
/// }
/// ```
pub async fn sla_alerts(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let total_breached: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE response_state = 'sla_breached'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let total_at_risk: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE response_state = 'sla_at_risk'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // Alerts: a simple per-state rollup.
    let alerts: Vec<Value> = vec![json!({
        "state": "breached",
        "count": total_breached,
    })];
    // Per-mailbox: count breached conversations grouped by mailbox_id.
    let per_mailbox: Vec<Value> = conn
        .prepare("SELECT mailbox_id, COUNT(*) FROM conversations WHERE response_state = 'sla_breached' GROUP BY mailbox_id ORDER BY 2 DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "mailbox_id": r.get::<_, i64>(0)?,
                    "breached_count": r.get::<_, i64>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    // Unconfigured mailboxes: those without SLA configs.
    let unconfigured_mailboxes: Vec<Value> = conn
        .prepare("SELECT DISTINCT m.id, m.name FROM mailboxes m LEFT JOIN sla_configs s ON s.mailbox_id = m.id WHERE s.mailbox_id IS NULL ORDER BY m.id")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "total_breached": total_breached,
        "total_at_risk": total_at_risk,
        "alerts": alerts,
        "per_mailbox": per_mailbox,
        "unconfigured_mailboxes": unconfigured_mailboxes,
        "note": "Alerts measure BUSINESS minutes (nights/weekends excluded). SLA configs come from the sla_configs table."
    }))
}

/// GET /api/issues/clusters/:id
pub async fn get_cluster(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT id, name, status, created_at FROM issue_clusters WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Cluster not found"})),
    }
}

/// DELETE /api/issues/clusters/:id
pub async fn delete_cluster(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM issue_clusters WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// GET /api/issues/known
pub async fn list_known(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let issues = crate::intelligence_features::list_known_issues(&conn, None).unwrap_or_default();
    let items: Vec<Value> = issues
        .iter()
        .filter_map(|i| serde_json::to_value(i).ok())
        .collect();
    Json(json!({"known_issues": items}))
}

/// POST /api/issues/known
pub async fn create_known(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT INTO known_issues (name, status, description, created_at, updated_at) VALUES (?1, 'active', ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![name, description],
    );
    Json(json!({"ok": true}))
}

/// GET /api/issues/known/:id
pub async fn get_known(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::list_known_issues(&conn, Some("active")) {
        Ok(issues) => {
            if let Some(issue) = issues.iter().find(|i| i.id == Some(id)) {
                Json(serde_json::to_value(issue).unwrap_or(json!({})))
            } else {
                Json(json!({"error": "Known issue not found"}))
            }
        }
        Err(_) => Json(json!({"error": "Failed to list known issues"})),
    }
}

/// PATCH /api/issues/known/:id
pub async fn update_known(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        let _ = conn.execute("UPDATE known_issues SET name = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2", rusqlite::params![name, id]);
    }
    if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
        let _ = conn.execute("UPDATE known_issues SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2", rusqlite::params![status, id]);
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/issues/known/:id
pub async fn delete_known(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM known_issues WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/issues/known/:id/link/:conversationId
pub async fn link_known(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT OR IGNORE INTO known_issue_links (known_issue_id, conversation_id, link_type) VALUES (?1, ?2, 'manual')",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// DELETE /api/issues/known/:id/link/:conversationId
pub async fn unlink_known(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM known_issue_links WHERE known_issue_id = ?1 AND conversation_id = ?2",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/issues/known/:id/refs
pub async fn add_ref(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "id": id}))
}

/// GET /api/issues/cases
pub async fn list_cases(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"cases": []}))
}

/// POST /api/issues/cases/from-conversation/:conversationId
pub async fn case_from_conversation(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    Json(json!({"ok": true, "conversationId": conversation_id}))
}
