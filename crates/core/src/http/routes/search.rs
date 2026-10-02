//! Search routes — mirrors src/server/routes/search.ts

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// POST /api/search — universal search (FTS5).
pub async fn search(State(state): State<AppState>, Json(body): Json<Value>) -> impl IntoResponse {
    let query = body
        .get("query")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("q").and_then(|v| v.as_str()))
        .unwrap_or("");
    if query.trim().is_empty() {
        return Json(
            json!({"hits": [], "total": 0, "query": query, "used_semantic": false, "semantic_available": false}),
        );
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = crate::search::apply_fts_migration(&conn);
    match crate::search::universal_search(&conn, query) {
        Ok(results) => {
            let items: Vec<Value> = results
                .iter()
                .filter_map(|r| serde_json::to_value(r).ok())
                .collect();
            let total = items.len() as i64;
            // The port uses FTS5 only (no Qdrant Edge by default), so
            // `used_semantic` is always false and `semantic_available`
            // is false unless the `qdrant` cargo feature is enabled.
            Json(json!({
                "hits": items,
                "total": total,
                "query": query,
                "used_semantic": false,
                "semantic_available": cfg!(feature = "qdrant"),
            }))
        }
        Err(e) => Json(json!({
            "_status": 500,
            "message": e.to_string(),
            "hits": [],
            "total": 0,
            "query": query,
            "used_semantic": false,
            "semantic_available": cfg!(feature = "qdrant"),
        })),
    }
}
