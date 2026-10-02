//! collaboration routes — mirrors src/server/routes/collaboration.ts

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
pub async fn list_side_threads(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn create_side_thread(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn get_side_thread(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn add_message(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn resolve(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn reopen(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn mention_directory(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
