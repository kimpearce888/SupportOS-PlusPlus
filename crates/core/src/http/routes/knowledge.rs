//! knowledge routes — mirrors src/server/routes/knowledge.ts

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
pub async fn list_sources(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn list_documents(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn get_document(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn search(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn freshness(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn reindex(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn import(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn importable(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
