//! Search routes — mirrors src/server/routes/search.ts

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// POST /api/search — universal search (FTS5).
pub async fn search(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.trim().is_empty() {
        return (axum::http::StatusCode::OK, Json(json!({"results": []})));
    }
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = crate::search::apply_fts_migration(&conn);
    match crate::search::universal_search(&conn, query) {
        Ok(results) => {
            let items: Vec<Value> = results.iter().filter_map(|r| serde_json::to_value(r).ok()).collect();
            Json(json!({"results": items}))
        }
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
    }
}
