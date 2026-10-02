//! HTTP API route handlers — mirrors the reference's 32 route files.
//!
//! Each module corresponds to a reference route file.
//! Handlers take `AppState` and return `impl IntoResponse`.

#[allow(warnings)]
pub mod ai;
#[allow(warnings)]
pub mod analytics;
#[allow(warnings)]
pub mod attributes;
#[allow(warnings)]
pub mod automation;
#[allow(warnings)]
pub mod coaching;
#[allow(warnings)]
pub mod collaboration;
#[allow(warnings)]
pub mod connectors;
#[allow(warnings)]
pub mod conversations;
#[allow(warnings)]
pub mod copilot;
#[allow(warnings)]
pub mod custom_objects;
#[allow(warnings)]
pub mod docs;
#[allow(warnings)]
pub mod events;
#[allow(warnings)]
pub mod graph;
#[allow(warnings)]
pub mod incidents;
#[allow(warnings)]
pub mod interactions;
#[allow(warnings)]
pub mod issues;
#[allow(warnings)]
pub mod knowledge;
#[allow(warnings)]
pub mod memory;
#[allow(warnings)]
pub mod notifications;
#[allow(warnings)]
pub mod oauth;
#[allow(warnings)]
pub mod operations;
#[allow(warnings)]
pub mod outreach;
#[allow(warnings)]
pub mod people;
#[allow(warnings)]
pub mod quality;
#[allow(warnings)]
pub mod search;
#[allow(warnings)]
pub mod settings;
#[allow(warnings)]
pub mod sync;
#[allow(warnings)]
pub mod system;
#[allow(warnings)]
pub mod translation;
#[allow(warnings)]
pub mod views;
#[allow(warnings)]
pub mod webhook;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// 404 JSON response for unknown routes.
pub async fn not_found() -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        [("Content-Type", "application/json")],
        r#"{"statusCode":404,"error":"NotFound","message":"Unknown API endpoint."}"#,
    )
        .into_response()
}
