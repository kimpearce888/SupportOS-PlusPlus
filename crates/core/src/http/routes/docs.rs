//! Docs routes — mirrors src/server/routes/docs.ts
//!
//! Documents knowledge base — collections, articles, search. Stored in
//! the `knowledge_doc_freshness` table (the port's unified knowledge store).

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/docs/collections — list all doc collections (grouped by source).
pub async fn collections(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let collections: Vec<Value> = conn
        .prepare(
            "SELECT source, COUNT(*) FROM knowledge_doc_freshness GROUP BY source ORDER BY source",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "name": r.get::<_, String>(0)?,
                    "article_count": r.get::<_, i64>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "collections": collections,
        "docs_sync_available": false,
    }))
}

/// GET /api/docs/stats — aggregate stats over all docs.
pub async fn stats(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM knowledge_doc_freshness", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let fresh: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM knowledge_doc_freshness WHERE freshness_status = 'fresh'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let stale: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM knowledge_doc_freshness WHERE freshness_status = 'stale'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let sources: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT source) FROM knowledge_doc_freshness",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({
        "total_articles": total,
        "fresh": fresh,
        "stale": stale,
        "sources": sources,
        "articles": total,
        "collections": sources,
        "drafts": 0,
        "internal": 0,
        "published": total,
        "email_conversations": 0,
        "chat_sessions": 0,
        "total_views": 0,
        "last_synced_at": null,
        "docs_chunks": 0,
        "docs_chunks_indexed": 0,
        "docs_chunks_pending": 0,
        "docs_chunks_failed": 0,
    }))
}

/// GET /api/docs/search — full-text search over docs.
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let q = params.get("q").cloned().unwrap_or_default();
    if q.trim().is_empty() {
        return Json(json!({"results": [], "query": q}));
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // LIKE-based search of the doc titles. The FTS5 search module indexes
    // conversations + customers, not docs; for docs we fall back to LIKE.
    let pattern = format!("%{q}%");
    let results: Vec<Value> = conn
        .prepare("SELECT id, title, source FROM knowledge_doc_freshness WHERE title LIKE ?1 ORDER BY id DESC LIMIT 50")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![pattern], |r| {
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
    Json(json!({"results": results, "query": q}))
}

/// GET /api/docs/articles — list all articles with pagination.
pub async fn articles(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u32>().ok())
        .unwrap_or(50);
    let source = params.get("source").cloned();
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let articles: Vec<Value> = if let Some(s) = source.as_ref() {
        conn.prepare("SELECT id, title, source, freshness_status, last_reviewed_at FROM knowledge_doc_freshness WHERE source = ?1 ORDER BY id DESC LIMIT ?2")
            .ok()
            .map(|mut stmt| {
                stmt.query_map(rusqlite::params![s, limit], |r| {
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
            .unwrap_or_default()
    } else {
        conn.prepare("SELECT id, title, source, freshness_status, last_reviewed_at FROM knowledge_doc_freshness ORDER BY id DESC LIMIT ?1")
            .ok()
            .map(|mut stmt| {
                stmt.query_map(rusqlite::params![limit], |r| {
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
            .unwrap_or_default()
    };
    Json(json!({
        "articles": articles,
        "total": articles.len(),
        "page": 1,
        "page_size": limit,
    }))
}

/// GET /api/docs/articles/:id — single article by id.
pub async fn article(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT id, title, source, freshness_status, last_reviewed_at FROM knowledge_doc_freshness WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "title": r.get::<_, String>(1)?,
                "source": r.get::<_, String>(2)?,
                "freshness_status": r.get::<_, String>(3)?,
                "last_reviewed_at": r.get::<_, Option<String>>(4)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Article not found"})),
    }
}
