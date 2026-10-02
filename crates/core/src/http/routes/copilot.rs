//! copilot routes — mirrors src/server/routes/copilot.ts

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
pub async fn list_sessions(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn chat(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn get_session(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn delete_session(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn tools(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
