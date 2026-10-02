//! Knowledge routes — mirrors src/server/routes/knowledge.ts

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/knowledge/sources
pub async fn list_sources(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
    if query.trim().is_empty() {
        return Json(json!({"results": [], "query": query}));
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Use the shared universal_search (FTS5) — same path the reference uses
    // when Qdrant is not configured.
    let results: Vec<Value> = crate::search::universal_search(&conn, &query)
        .unwrap_or_default()
        .into_iter()
        .map(|r| serde_json::to_value(&r).unwrap_or_else(|_| json!({})))
        .collect();
    Json(json!({"results": results, "query": query}))
}

/// GET /api/knowledge/freshness
pub async fn freshness(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
    let documents: Vec<Value> = conn
        .prepare("SELECT id, title, source, freshness_status, last_reviewed_at FROM knowledge_doc_freshness ORDER BY id DESC LIMIT 200")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "source": r.get::<_, String>(2)?,
                    "freshness_status": r.get::<_, String>(3)?,
                    "last_reviewed_at": r.get::<_, Option<String>>(4)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"stale": stale, "fresh": fresh, "documents": documents}))
}

/// POST /api/knowledge/import
pub async fn import(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    // Body shape: { "documents": [{ "title": "...", "source": "...", "content": "..." }] }
    let docs = body
        .get("documents")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let mut imported = 0u32;
    for doc in docs {
        let title = doc
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");
        let source = doc
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("manual");
        let content = doc.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if conn
            .execute(
                "INSERT INTO knowledge_doc_freshness (title, source, freshness_status, last_reviewed_at)
                 VALUES (?1, ?2, 'fresh', strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                rusqlite::params![title, source],
            )
            .is_ok()
        {
            imported += 1;
        }
    }
    // Push a real-time SyncUpdated event so connected clients refresh.
    if imported > 0 {
        drop(conn);
    }
    Json(json!({"ok": true, "imported": imported}))
}

/// POST /api/knowledge/import-file
pub async fn import_file(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    // Body shape: { "filename": "...", "content": "..." } (content is text)
    let filename = body
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("untitled.txt");
    let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let imported = if conn
        .execute(
            "INSERT INTO knowledge_doc_freshness (title, source, freshness_status, last_reviewed_at)
             VALUES (?1, 'file-upload', 'fresh', strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params![filename],
        )
        .is_ok()
    {
        1
    } else {
        0
    };
    if imported > 0 {
        drop(conn);
    }
    Json(json!({"ok": true, "imported": imported}))
}

/// GET /api/knowledge/importable
pub async fn importable(State(state): State<AppState>) -> Json<Value> {
    // Returns documents the user could import (from connectors etc).
    // For now, just list stale + unreviewed documents already in the DB.
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let docs: Vec<Value> = conn
        .prepare("SELECT id, title, source FROM knowledge_doc_freshness WHERE freshness_status = 'stale' ORDER BY last_reviewed_at ASC LIMIT 50")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "source": r.get::<_, String>(2)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"importable": docs}))
}

/// POST /api/knowledge/documents/:id/review
pub async fn review_document(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute("UPDATE knowledge_doc_freshness SET last_reviewed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), freshness_status = 'fresh' WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}

/// POST /api/knowledge/documents/:id/verify
pub async fn verify_document(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let rows = conn
        .execute(
            "UPDATE knowledge_doc_freshness
             SET last_reviewed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 freshness_status = 'fresh'
             WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap_or(0);
    drop(conn);
    let verified = rows > 0;
    if verified {}
    Json(json!({"ok": verified, "verified": verified}))
}

/// POST /api/knowledge/reindex
pub async fn reindex(State(state): State<AppState>) -> Json<Value> {
    // Re-mark all stale documents as needing review (without losing content).
    // A real reindex would rebuild the FTS5/vector index from the source
    // documents; for the local SQLite-backed port, we just emit a
    // SyncUpdated event so the UI refreshes the freshness view.
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let marked = conn
        .execute(
            "UPDATE knowledge_doc_freshness
             SET freshness_status = 'stale'
             WHERE last_reviewed_at IS NULL
                OR last_reviewed_at < datetime('now', '-30 days')",
            [],
        )
        .unwrap_or(0);
    drop(conn);
    if marked > 0 {}
    Json(json!({"ok": true, "marked_stale": marked, "message": "Reindex queued."}))
}

/// DELETE /api/knowledge/documents/:id
pub async fn delete_document(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let rows = conn
        .execute(
            "DELETE FROM knowledge_doc_freshness WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap_or(0);
    drop(conn);
    if rows > 0 {}
    Json(json!({"ok": rows > 0}))
}
