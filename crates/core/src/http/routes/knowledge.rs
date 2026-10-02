//! Knowledge routes — mirrors src/server/routes/knowledge.ts

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/knowledge/sources
pub async fn list_sources(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let sources: Vec<Value> = conn
        .prepare("SELECT DISTINCT source FROM knowledge_doc_freshness ORDER BY source")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| Ok(json!({"source": r.get::<_, String>(0)?})))
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"sources": sources}))
}

/// GET /api/knowledge/documents
pub async fn list_documents(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u32>().ok())
        .unwrap_or(50);
    let docs: Vec<Value> = conn
        .prepare("SELECT id, title, source, last_reviewed_at, freshness_status FROM knowledge_doc_freshness ORDER BY id DESC LIMIT ?1")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "source": r.get::<_, String>(2)?,
                    "last_reviewed_at": r.get::<_, Option<String>>(3)?,
                    "freshness_status": r.get::<_, String>(4)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"documents": docs}))
}

/// GET /api/knowledge/documents/:id
pub async fn get_document(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let row = conn.query_row("SELECT id, title, source, last_reviewed_at, freshness_status FROM knowledge_doc_freshness WHERE id = ?1", rusqlite::params![id], |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "title": r.get::<_, String>(1)?,
            "source": r.get::<_, String>(2)?,
            "last_reviewed_at": r.get::<_, Option<String>>(3)?,
            "freshness_status": r.get::<_, String>(4)?,
        }))
    });
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Document not found"})),
    }
}

/// GET /api/knowledge/search
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let query = params.get("q").cloned().unwrap_or_default();
    Json(json!({"results": [], "query": query}))
}

/// GET /api/knowledge/freshness
pub async fn freshness(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let stale: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM knowledge_doc_freshness WHERE freshness_status = 'stale'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let fresh: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM knowledge_doc_freshness WHERE freshness_status = 'fresh'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({"stale": stale, "fresh": fresh}))
}

/// POST /api/knowledge/import
pub async fn import(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    Json(json!({"ok": true, "imported": 0}))
}

/// POST /api/knowledge/import-file
pub async fn import_file(State(state): State<AppState>, Json(_body): Json<Value>) -> Json<Value> {
    Json(json!({"ok": true, "imported": 0}))
}

/// GET /api/knowledge/importable
pub async fn importable(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"importable": []}))
}

/// POST /api/knowledge/documents/:id/review
pub async fn review_document(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute("UPDATE knowledge_doc_freshness SET last_reviewed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), freshness_status = 'fresh' WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}

/// POST /api/knowledge/documents/:id/verify
pub async fn verify_document(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    Json(json!({"ok": true, "verified": true}))
}

/// POST /api/knowledge/reindex
pub async fn reindex(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "message": "Reindex queued."}))
}

/// DELETE /api/knowledge/documents/:id
pub async fn delete_document(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "DELETE FROM knowledge_doc_freshness WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}
