//! Outreach routes — mirrors src/server/routes/outreach.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/outreach/meta
pub async fn meta(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let segments: i64 = conn
        .query_row("SELECT COUNT(*) FROM saved_segments", [], |r| r.get(0))
        .unwrap_or(0);
    let campaigns: i64 = conn
        .query_row("SELECT COUNT(*) FROM campaigns", [], |r| r.get(0))
        .unwrap_or(0);
    let dnc: i64 = conn
        .query_row("SELECT COUNT(*) FROM do_not_contact", [], |r| r.get(0))
        .unwrap_or(0);
    Json(json!({"segments": segments, "campaigns": campaigns, "dnc": dnc}))
}

/// POST /api/outreach/segments/preview
pub async fn preview_segment(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"matches": [], "count": 0}))
}

/// POST /api/outreach/segments/estimate
pub async fn estimate_segment(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"estimate": 0}))
}

/// POST /api/outreach/segments/suggest
pub async fn suggest_segment(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"suggestions": []}))
}

/// GET /api/outreach/segments
pub async fn list_segments(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let segments = crate::outreach::list_segments(&conn).unwrap_or_default();
    let items: Vec<Value> = segments
        .iter()
        .filter_map(|s| serde_json::to_value(s).ok())
        .collect();
    Json(json!({"segments": items}))
}

/// POST /api/outreach/segments
pub async fn create_segment(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let criteria = body.get("criteria").and_then(|v| v.as_str()).unwrap_or("");
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::outreach::create_segment(&conn, name, criteria) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// DELETE /api/outreach/segments/:id
pub async fn delete_segment(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "DELETE FROM saved_segments WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/outreach/campaigns
pub async fn create_campaign(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let segment_id = body.get("segmentId").and_then(|v| v.as_i64());
    let template = body
        .get("messageTemplate")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::outreach::create_campaign(&conn, name, segment_id, Some(template)) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// GET /api/outreach/campaigns
pub async fn list_campaigns(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let campaigns = crate::outreach::list_campaigns(&conn).unwrap_or_default();
    let items: Vec<Value> = campaigns
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    Json(json!({"campaigns": items}))
}

/// GET /api/outreach/campaigns/:id
pub async fn get_campaign(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let campaigns = crate::outreach::list_campaigns(&conn).unwrap_or_default();
    if let Some(campaign) = campaigns.iter().find(|c| c.id == Some(id)) {
        Json(serde_json::to_value(campaign).unwrap_or(json!({})))
    } else {
        Json(json!({"error": "Campaign not found"}))
    }
}
