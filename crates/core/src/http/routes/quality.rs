//! quality routes — mirrors src/server/routes/quality.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// Stub handler — returns empty data for now. Will be implemented with real crate:: calls.
fn empty_response(key: &str) -> impl IntoResponse {
    Json(json!({key: []}))
}

// Auto-generated stub functions to match the router
pub async fn list_gaps(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn rebuild_gaps(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn qa_overview(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn qa_rebuild(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn qa_conversation(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn qa_analyze(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn friction_overview(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn friction_rebuild(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
