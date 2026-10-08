//! Knowledge routes — mirrors src/server/routes/knowledge.ts
//!
//! The REAL knowledge store (knowledge_sources / knowledge_documents /
//! knowledge_chunks + fts_knowledge, M039 — reference migration 013)
//! through the [`crate::knowledge_store`] port of the reference
//! KnowledgeRepository + KnowledgeIngestor. The v1.x routes operated on
//! the legacy `knowledge_doc_freshness` side table and never touched the
//! documents search and the AI pipeline actually read.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use crate::knowledge_store;

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": message,
        })),
    )
        .into_response()
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "statusCode": 400,
            "error": "BadRequest",
            "message": message,
        })),
    )
        .into_response()
}

/// GET /api/knowledge/sources
pub async fn list_sources(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    Json(json!({"sources": knowledge_store::list_sources(&conn)}))
}

/// GET /api/knowledge/documents (?sourceId=N)
pub async fn list_documents(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let source_id = params.get("sourceId").and_then(|s| s.parse::<i64>().ok());
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    Json(json!({"documents": knowledge_store::list_documents(&conn, source_id)}))
}

/// GET /api/knowledge/documents/:id — the document + the honest related
/// reads (reference v2.2.1: distinct citing conversations, never a
/// self-matching FTS count).
pub async fn get_document(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let Some(document) = knowledge_store::get_document(&conn, id) else {
        return not_found("Document not found.");
    };
    let related_ticket_estimate = knowledge_store::related_ticket_estimate(&conn, id);
    let related_known_issues = knowledge_store::search_known_issues_by_title(
        &conn,
        document.get("title").and_then(|v| v.as_str()).unwrap_or(""),
    );
    Json(json!({
        "document": document,
        "related_ticket_estimate": related_ticket_estimate,
        "related_known_issues": related_known_issues,
    }))
    .into_response()
}

/// GET /api/knowledge/search (?q=&visibility=customer_safe)
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let query = params.get("q").cloned().unwrap_or_default();
    let visibility = params
        .get("visibility")
        .map(String::as_str)
        .filter(|v| *v == "customer_safe");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let results = knowledge_store::search_knowledge(&conn, &query, visibility);
    Json(json!({"results": results, "query": query}))
}

/// GET /api/knowledge/freshness — the reduced v2.0.0 report over the real
/// store (deterministic date flags; usage/conflict/ticket-correlation
/// flags are a documented follow-up).
pub async fn freshness(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    Json(knowledge_store::freshness_report(&conn))
}

/// POST /api/knowledge/import — the manual (pasted/JSON) import.
/// knowledgeImportRequestSchema: sourceName (default "Manual import"),
/// visibility enum (default internal_only), documents[] (title, content
/// min 1, format enum default markdown, at least one).
pub async fn import(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let source_name = body
        .get("sourceName")
        .and_then(|v| v.as_str())
        .unwrap_or("Manual import")
        .to_string();
    let visibility = match body.get("visibility") {
        None | Some(Value::Null) => "internal_only".to_string(),
        Some(Value::String(s)) if s == "customer_safe" || s == "internal_only" => s.clone(),
        Some(_) => return bad_request("visibility must be customer_safe or internal_only."),
    };
    let Some(documents) = body.get("documents").and_then(|v| v.as_array()) else {
        return bad_request("At least one document is required.");
    };
    if documents.is_empty() {
        return bad_request("At least one document is required.");
    }
    for doc in documents {
        let content_empty = doc
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::is_empty)
            .unwrap_or(true);
        if content_empty {
            return bad_request("Document content must not be empty.");
        }
        if let Some(format) = doc.get("format") {
            if !matches!(
                format.as_str(),
                Some("markdown") | Some("txt") | Some("html")
            ) {
                return bad_request("format must be markdown, txt or html.");
            }
        }
    }

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match knowledge_store::import_manual(&conn, &source_name, documents, &visibility) {
        Ok(results) => {
            let _ =
                crate::jobs::enqueue_on(&conn, "embeddings", "embed_knowledge_chunks", "{}", 4, 2);
            let _ = crate::audit::audit(
                &conn,
                &crate::audit::AuditEntry::user("knowledge_imported").with_after_state(json!({
                    "count": results.len(),
                    "visibility": visibility,
                })),
            );
            Json(json!({
                "ok": true,
                "imported": results.len(),
                "documents": results,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "message": format!("Import failed: {e}")})),
        )
            .into_response(),
    }
}

/// POST /api/knowledge/import-file — import a file (MD/TXT/CSV/JSON/HTML
/// natively; PDF/DOCX with real text extraction — KN-02) confined to the
/// knowledge-import folder.
pub async fn import_file(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(path) = body
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return bad_request("A file path is required.");
    };
    if path.len() > 1024 {
        return bad_request("A file path is required.");
    }
    let source_name = body
        .get("sourceName")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let visibility = match body.get("visibility") {
        None | Some(Value::Null) => "internal_only".to_string(),
        Some(Value::String(s)) if s == "customer_safe" || s == "internal_only" => s.clone(),
        Some(_) => return bad_request("visibility must be customer_safe or internal_only."),
    };

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match knowledge_store::import_file(
        &conn,
        &state.data_dir,
        path,
        source_name.as_deref(),
        &visibility,
    ) {
        Ok(results) => {
            let _ =
                crate::jobs::enqueue_on(&conn, "embeddings", "embed_knowledge_chunks", "{}", 4, 2);
            Json(json!({
                "ok": true,
                "imported": results.len(),
                "documents": results,
            }))
            .into_response()
        }
        Err(crate::error::Error::Validation(message)) => {
            // Path-safety violations and parse failures are 4xx/422, never 500.
            if message.starts_with("For safety") || message.starts_with("File not found") {
                bad_request(&message)
            } else {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"ok": false, "message": message})),
                )
                    .into_response()
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "message": format!("Import failed: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/knowledge/importable — the knowledge-import folder listing.
pub async fn importable(State(state): State<AppState>) -> Json<Value> {
    Json(knowledge_store::importable_files(&state.data_dir))
}

/// POST /api/knowledge/documents/:id/review — the human-only timestamp.
pub async fn review_document(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match knowledge_store::mark_reviewed(&conn, id) {
        Ok(true) => {
            let _ = crate::audit::audit(
                &conn,
                &crate::audit::AuditEntry::user("knowledge_reviewed")
                    .with_after_state(json!({ "id": id })),
            );
            Json(json!({
                "ok": true,
                "message": "Reviewed (timestamp recorded; nothing is published automatically)."
            }))
            .into_response()
        }
        Ok(false) => not_found("Document not found."),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "message": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/knowledge/documents/:id/verify — the human-only timestamp.
pub async fn verify_document(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match knowledge_store::mark_verified(&conn, id) {
        Ok(true) => {
            let _ = crate::audit::audit(
                &conn,
                &crate::audit::AuditEntry::user("knowledge_verified")
                    .with_after_state(json!({ "id": id })),
            );
            Json(json!({
                "ok": true,
                "message": "Verified (timestamp recorded; nothing is published automatically)."
            }))
            .into_response()
        }
        Ok(false) => not_found("Document not found."),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "message": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/knowledge/reindex — rebuild every FTS index from the source
/// tables (one transaction) and queue the embedding pass.
pub async fn reindex(State(state): State<AppState>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::search::rebuild_indexes(&conn) {
        Ok(_) => {
            let _ =
                crate::jobs::enqueue_on(&conn, "embeddings", "embed_knowledge_chunks", "{}", 4, 2);
            Json(json!({"ok": true, "message": "Reindexing queued."})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "message": format!("Reindex failed: {e}")})),
        )
            .into_response(),
    }
}

/// DELETE /api/knowledge/documents/:id — chunks + FTS rows + document,
/// atomically (reference v1.6.0: orphaned FTS rows kept matching searches
/// for deleted documents).
pub async fn delete_document(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match knowledge_store::delete_document(&conn, id) {
        Ok(_) => {
            let _ = crate::audit::audit(
                &conn,
                &crate::audit::AuditEntry::user("knowledge_deleted")
                    .with_after_state(json!({ "id": id })),
            );
            Json(json!({"ok": true, "message": "Document deleted."})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "message": e.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn make_state() -> (AppState, tempfile::TempDir) {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let state = AppState {
            conn: Arc::new(Mutex::new(conn)),
            data_dir: dir.path().to_path_buf(),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: crate::http::EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
            workers: None,
            qdrant: Arc::new(crate::vectorstore_qdrant::EmbeddedQdrant::new(
                "/tmp/spp-test-qdrant",
                "http://127.0.0.1:6333",
                false,
            )),
        };
        (state, dir)
    }

    async fn body_json<R: axum::response::IntoResponse>(response: R) -> (StatusCode, Value) {
        let response = response.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn import_validates_and_persists() {
        let (state, _dir) = make_state();
        // Missing documents array.
        let (status, _) = body_json(
            import(
                State(state_handle(&state)),
                Json(json!({"sourceName": "S"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Bad visibility.
        let (status, _) = body_json(
            import(
                State(state_handle(&state)),
                Json(
                    json!({"visibility": "secret", "documents": [{"title": "t", "content": "c"}]}),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Valid import lands in the real store and the sources listing.
        let (status, body) = body_json(
            import(
                State(state_handle(&state)),
                Json(json!({
                    "sourceName": "Manual import",
                    "documents": [{"title": "Pasted", "content": "body", "format": "markdown"}]
                })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["imported"], 1);
        let (status, body) =
            body_json(list_documents(State(state_handle(&state)), Query(Default::default())).await)
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["documents"].as_array().unwrap().len(), 1);
        assert_eq!(body["documents"][0]["title"], "Pasted");
    }

    fn state_handle(state: &AppState) -> AppState {
        AppState {
            conn: Arc::clone(&state.conn),
            data_dir: state.data_dir.clone(),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: crate::http::EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
            workers: None,
            qdrant: Arc::clone(&state.qdrant),
        }
    }

    #[tokio::test]
    async fn document_detail_includes_related_reads() {
        let (state, _dir) = make_state();
        let (_, body) = body_json(
            import(
                State(state_handle(&state)),
                Json(json!({"documents": [{"title": "Detail", "content": "c"}]})),
            )
            .await,
        )
        .await;
        let id = body["documents"][0]["documentId"].as_i64().unwrap();

        let (status, body) =
            body_json(get_document(State(state_handle(&state)), Path(id)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["document"]["title"], "Detail");
        assert_eq!(body["related_ticket_estimate"], 0);
        assert!(body["related_known_issues"].as_array().is_some());

        let (status, _) =
            body_json(get_document(State(state_handle(&state)), Path(999)).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_removes_from_store_and_fts() {
        let (state, _dir) = make_state();
        let (_, body) = body_json(
            import(
                State(state_handle(&state)),
                Json(json!({"documents": [{"title": "Bye", "content": "c"}]})),
            )
            .await,
        )
        .await;
        let id = body["documents"][0]["documentId"].as_i64().unwrap();

        let (status, body) =
            body_json(delete_document(State(state_handle(&state)), Path(id)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);

        let (_, body) =
            body_json(list_documents(State(state_handle(&state)), Query(Default::default())).await)
                .await;
        assert_eq!(body["documents"].as_array().unwrap().len(), 0);
        let fts: i64 = {
            let conn = state.conn.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM fts_knowledge", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(fts, 0);
    }

    #[tokio::test]
    async fn review_verify_stamp_or_404() {
        let (state, _dir) = make_state();
        let (_, body) = body_json(
            import(
                State(state_handle(&state)),
                Json(json!({"documents": [{"title": "RV", "content": "c"}]})),
            )
            .await,
        )
        .await;
        let id = body["documents"][0]["documentId"].as_i64().unwrap();

        let (status, _) =
            body_json(review_document(State(state_handle(&state)), Path(999)).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, body) =
            body_json(review_document(State(state_handle(&state)), Path(id)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        let (status, body) =
            body_json(verify_document(State(state_handle(&state)), Path(id)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);

        // The freshness report reflects the stamps.
        let (_, report) = body_json(freshness(State(state_handle(&state))).await).await;
        let row = &report["documents"][0];
        assert!(row["last_reviewed_at"].as_str().is_some());
        assert!(row["last_verified_at"].as_str().is_some());
    }

    #[tokio::test]
    async fn import_file_route_confines_to_the_import_folder() {
        let (state, dir) = make_state();
        let import_dir = dir.path().join("knowledge-import");
        std::fs::create_dir_all(&import_dir).unwrap();
        std::fs::write(import_dir.join("note.md"), "body").unwrap();

        let (status, body) = body_json(
            import_file(
                State(state_handle(&state)),
                Json(json!({"path": "note.md"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["imported"], 1);

        // Traversal is a 400 with the safety message.
        let (status, body) = body_json(
            import_file(
                State(state_handle(&state)),
                Json(json!({"path": "../secret.txt"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("knowledge-import"));
    }

    #[tokio::test]
    async fn reindex_rebuilds_and_reports() {
        let (state, _dir) = make_state();
        let (_, _) = body_json(
            import(
                State(state_handle(&state)),
                Json(json!({"documents": [{"title": "RI", "content": "c"}]})),
            )
            .await,
        )
        .await;
        let (status, body) = body_json(reindex(State(state_handle(&state))).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["message"], "Reindexing queued.");
        // The FTS row survived the rebuild.
        let fts: i64 = {
            let conn = state.conn.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM fts_knowledge", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(fts, 1);
    }
}
