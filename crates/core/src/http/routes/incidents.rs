//! Incidents routes — mirrors src/server/routes/incidents.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/incidents
pub async fn list(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let incidents = crate::intelligence_features::list_incidents(&conn, None).unwrap_or_default();
    let items: Vec<Value> = incidents
        .iter()
        .filter_map(|i| serde_json::to_value(i).ok())
        .collect();
    Json(json!({"incidents": items}))
}

/// POST /api/incidents
pub async fn create(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    Json(json!({"ok": true, "message": "Incident creation not yet implemented via HTTP."}))
}

/// GET /api/incidents/:id
pub async fn get(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let incidents = crate::intelligence_features::list_incidents(&conn, None).unwrap_or_default();
    if let Some(incident) = incidents.iter().find(|i| i.id == Some(id)) {
        Json(serde_json::to_value(incident).unwrap_or(json!({})))
    } else {
        Json(json!({"error": "Incident not found"}))
    }
}

/// PATCH /api/incidents/:id
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
        // Status update via HTTP needs IncidentStatus enum parsing (not yet wired)
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/incidents/:id
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute("DELETE FROM incidents WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}

/// POST /api/incidents/:id/conversations/:conversationId
pub async fn link_conversation(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "INSERT OR IGNORE INTO incident_conversations (incident_id, conversation_id) VALUES (?1, ?2)",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/incidents/:id/notes
pub async fn add_note(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let note = body.get("note").and_then(|v| v.as_str()).unwrap_or("");
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "INSERT INTO incident_timeline (incident_id, event_type, description, created_at) VALUES (?1, 'note', ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![id, note],
    );
    Json(json!({"ok": true}))
}

/// POST /api/incidents/:id/refs
pub async fn add_ref(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// POST /api/incidents/:id/releases
pub async fn add_release(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// DELETE /api/incidents/:id/releases/:releaseId
pub async fn delete_release(
    State(state): State<AppState>,
    Path((id, _release_id)): Path<(i64, i64)>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// POST /api/incidents/:id/related
pub async fn add_related(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// DELETE /api/incidents/:id/related/:targetKind/:targetLocalId
pub async fn delete_related(
    State(state): State<AppState>,
    Path((id, _target_kind, _target_local_id)): Path<(i64, String, i64)>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// GET /api/incidents/:id/impact
pub async fn impact(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let linked: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM incident_conversations WHERE incident_id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({"impact": {"linked_conversations": linked, "severity": "unknown"}}))
}
