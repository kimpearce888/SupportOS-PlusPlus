//! HTTP API route handlers — mirrors the reference's 32 route files.
//!
//! Each module corresponds to a reference route file.
//! Handlers take `AppState` and return `impl IntoResponse`.

pub mod system;
pub mod events;
pub mod conversations;
pub mod people;
pub mod search;
pub mod views;
pub mod operations;
pub mod notifications;
pub mod settings;
pub mod sync;
pub mod webhook;
pub mod analytics;
pub mod ai;
pub mod issues;
pub mod automation;
pub mod collaboration;
pub mod copilot;
pub mod knowledge;
pub mod outreach;
pub mod custom_objects;
pub mod connectors;
pub mod incidents;
pub mod graph;
pub mod attributes;
pub mod coaching;
pub mod translation;
pub mod memory;
pub mod interactions;
pub mod quality;
pub mod docs;

use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;

/// 404 JSON response for unknown routes.
pub async fn not_found() -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        [("Content-Type", "application/json")],
        r#"{"statusCode":404,"error":"NotFound","message":"Unknown API endpoint."}"#,
    )
        .into_response()
}
