//! Analytics routes — mirrors src/server/routes/analytics.ts

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/analytics/dashboard
pub async fn dashboard(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mailbox_id = params.get("mailboxId").and_then(|m| m.parse::<i64>().ok());
    let days_back = params.get("daysBack").and_then(|d| d.parse::<u32>().ok()).unwrap_or(7);
    match crate::reports::get_dashboard_metrics(&conn, mailbox_id, days_back) {
        Ok(metrics) => Json(serde_json::to_value(&metrics).unwrap_or(json!({}))),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
    }
}

/// GET /api/analytics/ai
pub async fn ai_analytics(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"ai_runs": 0, "ai_attributes": 0}))
}

/// GET /api/reports/sla
pub async fn sla_report(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let days_back = params.get("daysBack").and_then(|d| d.parse::<u32>().ok()).unwrap_or(7);
    match crate::reports::get_health_facts(&conn, days_back) {
        Ok(facts) => Json(serde_json::to_value(&facts).unwrap_or(json!({}))),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
    }
}

pub async fn why_contacting(State(state): State<AppState>) -> impl IntoResponse { Json(json!({"reasons": []})) }
pub async fn top_questions(State(state): State<AppState>) -> impl IntoResponse { Json(json!({"questions": []})) }
pub async fn doc_gaps(State(state): State<AppState>) -> impl IntoResponse { Json(json!({"gaps": []})) }
pub async fn answer_reuse(State(state): State<AppState>) -> impl IntoResponse { Json(json!({"reuse": []})) }
pub async fn issue_radar(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::intelligence_features::get_radar_snapshot(&conn) {
        Ok(snapshot) => {
            let alerts = vec![json!({
                "type": "radar_snapshot",
                "active_known_issues": snapshot.active_known_issues,
                "active_clusters": snapshot.active_clusters,
                "active_incidents": snapshot.active_incidents,
            })];
            Json(json!({"alerts": alerts}))
        }
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
    }
}
pub async fn metric_definitions(State(state): State<AppState>) -> impl IntoResponse { Json(json!({"definitions": []})) }
pub async fn release_correlation(State(state): State<AppState>) -> impl IntoResponse { Json(json!({"correlations": []})) }
pub async fn release_events(State(state): State<AppState>, Json(_body): Json<Value>) -> impl IntoResponse { Json(json!({"ok": true})) }
pub async fn helpscout_report(State(state): State<AppState>, axum::extract::Path(_key): axum::extract::Path<String>) -> impl IntoResponse { Json(json!({"data": []})) }
pub async fn narrative(State(state): State<AppState>, Json(_body): Json<Value>) -> impl IntoResponse { Json(json!({"narrative": "Not implemented."})) }
pub async fn effectiveness(State(state): State<AppState>) -> impl IntoResponse { Json(json!({"effectiveness": []})) }
pub async fn report_catalog(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::reports::get_health_facts(&conn, 7) {
        Ok(facts) => Json(serde_json::to_value(&facts).unwrap_or(json!({}))),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
    }
}
