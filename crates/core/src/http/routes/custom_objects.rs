//! custom_objects routes — mirrors src/server/routes/custom_objects.ts

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
pub async fn list_types(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn create_type(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn get_type(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn update_type(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn delete_type(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn list_objects(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn create_object(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn get_object(State(state): State<super::super::server::AppState>) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn update_object(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
pub async fn delete_object(
    State(state): State<super::super::server::AppState>,
) -> impl IntoResponse {
    Json(serde_json::json!({"data": []}))
}
