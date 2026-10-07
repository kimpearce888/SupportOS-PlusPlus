//! Docs routes — mirrors src/server/routes/docs.ts
//!
//! Read-only access to the local Docs API mirror (collections, categories,
//! articles + FTS search). The mirror is synced by the sync engine's
//! `docs_collections` / `docs_articles` resources using the separate Docs
//! API key; without that key the mirror stays empty and these endpoints
//! return empty lists (honest capability, no errors).
//!
//! v1.4.0 adds hybrid search: FTS5 + semantic retrieval fused with RRF
//! (Reciprocal Rank Fusion). Semantic vectors come from the embedded
//! Qdrant adapter when available, with a local cosine scan over stored
//! embeddings so the feature degrades honestly:
//! - no embedding model configured  -> FTS only, semantic_available=false
//! - model configured, no embeddings yet -> FTS only, honest note
//! - model configured, Qdrant serves nothing -> local cosine scan

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use rusqlite::Connection;
use serde_json::{json, Value};

use super::super::server::AppState;
use crate::embeddings;
use crate::hybrid_search::{merge_doc_hits, FtsDocHit, SemanticDocHit};

/// How many articles the FTS retriever streams (reference: 400 rows).
const FTS_ROW_LIMIT: i64 = 400;
/// How many articles the semantic retriever ranks (reference: 20).
const SEMANTIC_LIMIT: usize = 20;
/// Matched-chunk preview length (reference: `content.slice(0, 220)`).
const MATCHED_CHUNK_UNITS: usize = 220;

// Reference mode_note strings (docs.ts:53-61, exact).
const MODE_NOTE_DEFAULT: &str = "Keyword search (FTS5).";
const MODE_NOTE_SEMANTIC_DISABLED: &str =
    "Keyword search (FTS) - semantic retrieval disabled for this query.";
const MODE_NOTE_QDRANT: &str =
    "Hybrid search: FTS5 + semantic vectors via Qdrant, fused with Reciprocal Rank Fusion.";
const MODE_NOTE_LOCAL_COSINE: &str = "Hybrid search: FTS5 + semantic vectors (local cosine scan - Qdrant not reachable), fused with Reciprocal Rank Fusion.";
const MODE_NOTE_NO_MODEL: &str = "Keyword search (FTS5). Semantic search needs an embedding model: Settings → LM Studio → embedding model, then the docs embedding job runs on the next sync.";
const MODE_NOTE_NO_CHUNKS: &str = "Keyword search (FTS5). An embedding model is configured but no docs chunks are embedded yet - run a sync so the embedding job can process the mirror.";
const MODE_NOTE_EMBED_FAILED: &str = "Keyword search (FTS5). Semantic retrieval failed this request (embedding provider unreachable) - retried automatically next search.";

type Reply = (StatusCode, Json<Value>);

/// The shared summary/detail SELECT (reference `DocsArticleSummary` shape —
/// port column names mapped to reference payload names at the query
/// boundary, DB-04: `docs` -> docs_articles, `docs_collections`/
/// `docs_categories` joins for the denormalized names).
const ARTICLE_SUMMARY_SELECT: &str = "SELECT a.id, a.remote_id,
       a.collection_local_id, c.name,
       a.category_local_id, cat.name,
       a.number, a.slug, a.name, a.status, a.preview, a.words, a.views,
       a.remote_created_at, a.remote_updated_at
  FROM docs a
  LEFT JOIN docs_collections c ON c.id = a.collection_local_id
  LEFT JOIN docs_categories cat ON cat.id = a.category_local_id";

/// Render one summary row (order matches [`ARTICLE_SUMMARY_SELECT`]).
fn article_summary_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Value)> {
    Ok((
        r.get::<_, i64>(0)?,
        json!({
            "id": r.get::<_, i64>(0)?,
            "remote_id": r.get::<_, i64>(1)?,
            "collection_id": r.get::<_, i64>(2)?,
            "collection_name": r.get::<_, Option<String>>(3)?,
            "category_id": r.get::<_, Option<i64>>(4)?,
            "category_name": r.get::<_, Option<String>>(5)?,
            "number": r.get::<_, Option<i64>>(6)?,
            "slug": r.get::<_, Option<String>>(7)?,
            "name": r.get::<_, String>(8)?,
            "status": r.get::<_, Option<String>>(9)?,
            "preview": r.get::<_, Option<String>>(10)?,
            "words": r.get::<_, Option<i64>>(11)?,
            "views": r.get::<_, Option<i64>>(12)?,
            "remote_created_at": r.get::<_, Option<String>>(13)?,
            "remote_updated_at": r.get::<_, Option<String>>(14)?,
        }),
    ))
}

/// The reference 422 envelope (single message, no issue list — docs.ts).
fn validation_error(message: &str) -> Reply {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": message,
        })),
    )
}

/// JS `Number(value)` semantics for a query param, where an ABSENT or
/// EMPTY-STRING param means "use the route default" (the reference's
/// falsy check): `None` for those, `Some(number)` otherwise. Blank-but-
/// non-empty strings parse like JS `Number(' ')` = 0 (whitespace is
/// trimmed before parsing); garbage parses to NaN, which fails every
/// integer/range check exactly like the reference.
fn js_number(value: Option<&String>) -> Option<f64> {
    match value {
        None => None,
        Some(s) if s.is_empty() => None,
        Some(s) => Some(s.trim().parse::<f64>().unwrap_or(f64::NAN)),
    }
}

/// JS `Number.isInteger`.
fn is_js_integer(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0
}

/// FTS hits as (articleId -> best rank position + snippet) over `docs_fts`
/// (reference `ftsDocRanks`): tokens longer than 1 char capped at 8,
/// quoted-prefix matched; rows stream in rank order and the FIRST row per
/// article wins (fts5 `snippet` cannot combine with GROUP BY).
fn fts_doc_ranks(conn: &Connection, q: &str) -> (Vec<FtsDocHit>, HashMap<i64, String>) {
    // Reference tokenization: strip ["*()], whitespace-split, len > 1, cap 8.
    let tokens = crate::search::tokenize(q, 2, 8);
    if tokens.is_empty() {
        return (Vec::new(), HashMap::new());
    }
    let pattern = tokens
        .iter()
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut stmt = match conn.prepare(
        "SELECT article_id, snippet(docs_fts, 1, '[', ']', '…', 14)
           FROM docs_fts WHERE docs_fts MATCH ?1 ORDER BY rank LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return (Vec::new(), HashMap::new()),
    };
    let rows = stmt
        .query_map(rusqlite::params![pattern, FTS_ROW_LIMIT], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut hits: Vec<FtsDocHit> = Vec::new();
    let mut snippets: HashMap<i64, String> = HashMap::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for (article_id, snippet) in rows {
        if seen.insert(article_id) {
            hits.push(FtsDocHit {
                article_id,
                rank: hits.len(),
            });
            if let Some(s) = snippet {
                snippets.insert(article_id, s);
            }
        }
    }
    (hits, snippets)
}

/// GET /api/docs/collections — list all doc collections of the mirror
/// (reference `listCollections`, ordered by name).
pub async fn collections(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn_lock();
    let collections: Vec<Value> = conn
        .prepare(
            "SELECT id, remote_id, name, slug, description, visibility,
                    article_count, last_synced_at
               FROM docs_collections ORDER BY name",
        )
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "remote_id": r.get::<_, i64>(1)?,
                    "name": r.get::<_, String>(2)?,
                    "slug": r.get::<_, Option<String>>(3)?,
                    "description": r.get::<_, Option<String>>(4)?,
                    "visibility": r.get::<_, Option<String>>(5)?,
                    "article_count": r.get::<_, Option<i64>>(6)?,
                    "last_synced_at": r.get::<_, Option<String>>(7)?,
                }))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    // Reference: `ctx.provider.kind === 'fake' || ctx.realProvider?.docs
    // !== undefined` — the real provider's docs namespace exists only when
    // the separate Docs API key is configured.
    let docs_sync_available = state.provider_kind == "fake"
        || state
            .real
            .as_ref()
            .is_some_and(|r| !r.credentials.docs_api_key.is_empty());
    Json(json!({
        "collections": collections,
        "docs_sync_available": docs_sync_available,
    }))
}

/// GET /api/docs/stats — aggregate stats over the docs mirror
/// (reference `docs.stats()`).
pub async fn stats(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn_lock();
    let (collections, articles, published, drafts, internal, total_views, last_synced_at) = conn
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM docs_collections),
               (SELECT COUNT(*) FROM docs),
               (SELECT COUNT(*) FROM docs WHERE status = 'published'),
               (SELECT COUNT(*) FROM docs WHERE status = 'draft'),
               (SELECT COUNT(*) FROM docs WHERE status = 'internal'),
               (SELECT COALESCE(SUM(views), 0) FROM docs),
               (SELECT MAX(last_synced_at) FROM docs)",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .unwrap_or((0, 0, 0, 0, 0, 0, None));
    // Reference: chat/email conversation counts with the soft-delete filter.
    let (chat_sessions, email_conversations): (i64, i64) = conn
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM conversations
                 WHERE deleted_at IS NULL AND type = 'chat'),
               (SELECT COUNT(*) FROM conversations
                 WHERE deleted_at IS NULL AND type = 'email')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    // v1.4.0 semantic-search readiness (may be 0 until an embedding model
    // runs).
    let embedding = embeddings::docs_chunk_stats(&conn).unwrap_or_default();
    Json(json!({
        "collections": collections,
        "articles": articles,
        "published": published,
        "drafts": drafts,
        "internal": internal,
        "total_views": total_views,
        "last_synced_at": last_synced_at,
        "chat_sessions": chat_sessions,
        "email_conversations": email_conversations,
        "docs_chunks": embedding.chunks,
        "docs_chunks_indexed": embedding.indexed,
        "docs_chunks_pending": embedding.pending,
        "docs_chunks_failed": embedding.failed,
    }))
}

/// GET /api/docs/search — hybrid FTS5 + semantic docs search fused with
/// RRF (reference docs.ts:29-123).
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Reply {
    // limit: zod-equivalent integer 1-100, default 25.
    let limit = match js_number(params.get("limit")) {
        None => 25_i64,
        Some(n) if is_js_integer(n) && (1.0..=100.0).contains(&n) => n as i64,
        Some(_) => {
            return validation_error("limit must be 1-100.");
        }
    };
    // q is required (trimmed, like the reference).
    let query = params
        .get("q")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if query.is_empty() {
        return validation_error("q is required.");
    }
    // semantic: on unless the param is present and not ''/'1'/'true'.
    let semantic_requested = match params.get("semantic").map(|s| s.as_str()) {
        None | Some("") | Some("1") | Some("true") => true,
        Some(_) => false,
    };

    // Everything that needs the connection guard happens in this block; it
    // ends before the embed await (a MutexGuard held across an await makes
    // the handler future !Send).
    let (fts, snippets, settings, qdrant_enabled, stats) = {
        let conn = state.conn_lock();
        let (fts, snippets) = fts_doc_ranks(&conn, &query);
        let settings = embeddings::lm_studio_embedding_settings(&conn).unwrap_or(
            embeddings::LmStudioEmbeddingSettings {
                base_url: String::new(),
                embedding_model: None,
                timeout_ms: 0,
            },
        );
        let qdrant_enabled =
            crate::settings::get_bool(&conn, "qdrant_enabled", true).unwrap_or(true);
        let stats = embeddings::docs_chunk_stats(&conn).unwrap_or_default();
        (fts, snippets, settings, qdrant_enabled, stats)
    };

    // Reference gate order: semanticRequested, then model, then indexed.
    let mut semantic: Vec<SemanticDocHit> = Vec::new();
    let mut matched_chunks: HashMap<i64, String> = HashMap::new();
    let mut used_semantic = false;
    let mut mode_note = MODE_NOTE_DEFAULT.to_string();

    if !semantic_requested {
        mode_note = MODE_NOTE_SEMANTIC_DISABLED.to_string();
    } else if settings.embedding_model.is_none() {
        mode_note = MODE_NOTE_NO_MODEL.to_string();
    } else if stats.indexed == 0 {
        mode_note = MODE_NOTE_NO_CHUNKS.to_string();
    } else if let Some(model) = settings.embedding_model.clone() {
        // Reference embedQuery(): one embed call per request; a failure is
        // caught and the hybrid layer silently falls back to FTS with an
        // honest note.
        let embedded = embeddings::embed_texts(
            &settings.base_url,
            &model,
            &[query.clone()],
            settings.timeout_ms,
        )
        .await;
        match embedded {
            Ok(vectors) if vectors.first().is_some_and(|v| !v.is_empty()) => {
                let vector = vectors.into_iter().next().unwrap_or_default();
                // Qdrant first (ANN, same vectors). The reference does not
                // dedup articles here (a multi-chunk article may contribute
                // several rank positions) — parity keeps that behavior.
                if qdrant_enabled {
                    for h in state.qdrant.search(&vector, SEMANTIC_LIMIT) {
                        if h.payload.get("entity_type").and_then(|v| v.as_str())
                            != Some("docs_chunk")
                        {
                            continue;
                        }
                        let Some(article_id) = h.payload.get("entity_id").and_then(|v| v.as_i64())
                        else {
                            continue;
                        };
                        semantic.push(SemanticDocHit {
                            article_id,
                            score: h.score,
                        });
                    }
                }
                let served_by_qdrant = !semantic.is_empty();
                if semantic.is_empty() {
                    // Local cosine fallback over stored embeddings (works
                    // without Qdrant): best chunk per article, top 20.
                    let conn = state.conn_lock();
                    if let Ok(chunks) = embeddings::list_doc_chunks_with_embedding(&conn) {
                        let mut best: Vec<(i64, f32)> = Vec::new();
                        let mut index: HashMap<i64, usize> = HashMap::new();
                        for c in &chunks {
                            let sim =
                                crate::hybrid_search::cosine_similarity(&vector, &c.embedding);
                            match index.get(&c.article_id) {
                                Some(&i) => {
                                    if sim > best[i].1 {
                                        best[i].1 = sim;
                                        matched_chunks.insert(
                                            c.article_id,
                                            embeddings::utf16_slice(
                                                &c.content,
                                                0,
                                                MATCHED_CHUNK_UNITS,
                                            ),
                                        );
                                    }
                                }
                                None => {
                                    index.insert(c.article_id, best.len());
                                    best.push((c.article_id, sim));
                                    matched_chunks.insert(
                                        c.article_id,
                                        embeddings::utf16_slice(&c.content, 0, MATCHED_CHUNK_UNITS),
                                    );
                                }
                            }
                        }
                        best.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        semantic = best
                            .into_iter()
                            .take(SEMANTIC_LIMIT)
                            .map(|(article_id, score)| SemanticDocHit { article_id, score })
                            .collect();
                    }
                }
                if served_by_qdrant {
                    mode_note = MODE_NOTE_QDRANT.to_string();
                } else {
                    mode_note = MODE_NOTE_LOCAL_COSINE.to_string();
                }
                used_semantic = !semantic.is_empty();
            }
            _ => {
                mode_note = MODE_NOTE_EMBED_FAILED.to_string();
            }
        }
    }

    let merged = merge_doc_hits(&fts, &semantic, usize::try_from(limit).unwrap_or(25));

    // getArticlesByIds: one batched query, then the merged order wins
    // (missing rows are skipped, reference `continue`).
    let mut hits: Vec<Value> = Vec::new();
    {
        let conn = state.conn_lock();
        let by_id: HashMap<i64, Value> = if merged.is_empty() {
            HashMap::new()
        } else {
            let list = merged
                .iter()
                .map(|m| m.article_id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            conn.prepare(&format!("{ARTICLE_SUMMARY_SELECT} WHERE a.id IN ({list})"))
                .and_then(|mut stmt| {
                    stmt.query_map([], |r| article_summary_json(r))
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default()
        };
        for m in &merged {
            let Some(article) = by_id.get(&m.article_id) else {
                continue;
            };
            let snippet = snippets.get(&m.article_id).cloned().or_else(|| {
                article
                    .get("preview")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
            hits.push(json!({
                "article": article,
                "score": m.score,
                "why": m.why,
                "snippet": snippet,
                "matched_chunk": matched_chunks.get(&m.article_id).cloned(),
            }));
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "query": query,
            "hits": hits,
            "total": hits.len(),
            "used_semantic": used_semantic,
            "semantic_available": settings.embedding_model.is_some() && stats.indexed > 0,
            "mode_note": mode_note,
        })),
    )
}

/// GET /api/docs/articles — paged article list with optional collection
/// filter, FTS search and status filter (reference docs.ts:125-151 +
/// `listArticles`).
pub async fn articles(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Reply {
    // page >= 1 (default 1), pageSize 1-100 (default 25) — one combined
    // 422 message, like the reference.
    let page_n = js_number(params.get("page")).unwrap_or(1.0);
    let page_size_n = js_number(params.get("pageSize")).unwrap_or(25.0);
    if !is_js_integer(page_n)
        || page_n < 1.0
        || !is_js_integer(page_size_n)
        || !(1.0..=100.0).contains(&page_size_n)
    {
        return validation_error("page must be >= 1 and pageSize must be 1-100.");
    }
    // collectionId: positive integer when present (blank = absent).
    let collection_id = match params.get("collectionId") {
        None => None,
        Some(s) if s.is_empty() => None,
        Some(_) => {
            let n = js_number(params.get("collectionId")).unwrap_or(f64::NAN);
            if is_js_integer(n) && n > 0.0 {
                Some(n as i64)
            } else {
                return validation_error("collectionId must be a positive integer.");
            }
        }
    };
    // status: one of the three reference values (blank = absent, like the
    // reference's falsy handling).
    let status = params.get("status").filter(|s| !s.is_empty());
    if let Some(status) = status {
        if !matches!(&**status, "published" | "draft" | "internal") {
            return validation_error("status must be one of 'published', 'draft', 'internal'.");
        }
    }

    let conn = state.conn_lock();

    // WHERE assembly (reference listArticles): collection, status, then the
    // FTS-derived id set (`q` tokens, cap 6 — search uses 8).
    let mut where_parts: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(collection) = collection_id {
        where_parts.push(format!("a.collection_local_id = ?{}", binds.len() + 1));
        binds.push(Box::new(collection));
    }
    if let Some(status) = status {
        where_parts.push(format!("a.status = ?{}", binds.len() + 1));
        binds.push(Box::new(status.to_string()));
    }
    if let Some(q) = params.get("q").filter(|s| !s.is_empty()) {
        let tokens = crate::search::tokenize(q, 2, 6);
        if !tokens.is_empty() {
            let pattern = tokens
                .iter()
                .map(|t| format!("\"{t}\"*"))
                .collect::<Vec<_>>()
                .join(" ");
            let ids: Vec<i64> = conn
                .prepare("SELECT article_id FROM docs_fts WHERE docs_fts MATCH ?1")
                .and_then(|mut stmt| {
                    stmt.query_map(rusqlite::params![pattern], |r| r.get::<_, i64>(0))
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();
            if ids.is_empty() {
                where_parts.push("0".to_string());
            } else {
                let list = ids
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                where_parts.push(format!("a.id IN ({list})"));
            }
        }
    }
    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    // Repo-side clamps (defensive, like the reference repo).
    let page = page_n as i64;
    let page_size = page_size_n as i64;
    let offset = (page - 1) * page_size;

    let params_ref: Vec<&dyn rusqlite::types::ToSql> = binds.iter().map(|v| v.as_ref()).collect();
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM docs a {where_sql}"),
            params_ref.as_slice(),
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut all_binds = binds;
    all_binds.push(Box::new(page_size));
    all_binds.push(Box::new(offset));
    let all_refs: Vec<&dyn rusqlite::types::ToSql> = all_binds.iter().map(|v| v.as_ref()).collect();
    let article_rows: Vec<Value> = conn
        .prepare(&format!(
            "{ARTICLE_SUMMARY_SELECT} {where_sql}
             ORDER BY COALESCE(a.remote_updated_at, a.remote_created_at) DESC, a.id DESC
             LIMIT ?{} OFFSET ?{}",
            all_binds.len() - 1,
            all_binds.len(),
        ))
        .and_then(|mut stmt| {
            stmt.query_map(all_refs.as_slice(), |r| {
                article_summary_json(r).map(|(_, v)| v)
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    (
        StatusCode::OK,
        Json(json!({
            "articles": article_rows,
            "total": total,
            "page": page,
            "page_size": page_size,
        })),
    )
}

/// GET /api/docs/articles/:id — single article by id (reference
/// docs.ts:153-165 + `getArticle`): summary + text, 404 envelope with the
/// reference message when the mirror lacks the row.
pub async fn article(State(state): State<AppState>, Path(id): Path<i64>) -> Reply {
    if id <= 0 {
        return validation_error("Article id must be a positive integer.");
    }
    let conn = state.conn_lock();
    let row: Option<Value> = conn
        .query_row(
            &format!(
                "SELECT a.id, a.remote_id,
                        a.collection_local_id, c.name,
                        a.category_local_id, cat.name,
                        a.number, a.slug, a.name, a.status, a.preview, a.words, a.views,
                        a.remote_created_at, a.remote_updated_at, a.text
                   FROM docs a
                   LEFT JOIN docs_collections c ON c.id = a.collection_local_id
                   LEFT JOIN docs_categories cat ON cat.id = a.category_local_id
                  WHERE a.id = ?1"
            ),
            rusqlite::params![id],
            |r| {
                let (_, mut summary) = article_summary_json(r)?;
                summary["text"] = json!(r.get::<_, Option<String>>(15)?);
                Ok(summary)
            },
        )
        .ok();
    match row {
        Some(article) => (
            StatusCode::OK,
            Json(json!({
                "article": article,
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Article not found in the local docs mirror. Run a sync with a Docs API key configured."
            })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn fresh_db() -> Connection {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        // The canonical boot chain — docs mirror tables, docs_fts, chunk
        // tables with embedding_attempts, everything.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn make_state() -> (AppState, Arc<Mutex<Connection>>) {
        let conn = fresh_db();
        // Qdrant disabled in settings AND adapter disabled — a coherent
        // state (production constructs both from the same setting), so the
        // semantic retriever runs its local cosine scan.
        crate::settings::set_bool(&conn, "qdrant_enabled", false).unwrap();
        let conn = Arc::new(Mutex::new(conn));
        let state = AppState {
            conn: conn.clone(),
            data_dir: std::path::PathBuf::from("/tmp"),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: crate::http::EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
            workers: None,
            qdrant: std::sync::Arc::new(crate::vectorstore_qdrant::EmbeddedQdrant::new(
                "/tmp/spp-test-qdrant",
                "http://127.0.0.1:6333",
                false,
            )),
        };
        (state, conn)
    }

    /// Seed the docs mirror the way the sync engine's upserts do (two
    /// collections, two categories, three articles incl. one draft) plus
    /// the docs_fts rows.
    fn seed_mirror(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO docs_collections (id, remote_id, slug, name, description, visibility, article_count, last_synced_at) VALUES
                (1, 801, 'getting-started', 'Getting Started', 'First steps', 'public', 2, '2026-10-06 10:00:00'),
                (2, 802, 'billing', 'Billing & Account', 'Plans and invoices', 'public', 1, '2026-10-06 10:05:00');
             INSERT INTO docs_categories (id, remote_id, collection_local_id, slug, name) VALUES
                (1, 851, 1, 'setup', 'Setup'),
                (2, 853, 2, 'invoices', 'Invoices');
             INSERT INTO docs (id, remote_id, collection_local_id, category_local_id, number, slug, name, status, preview, text, views, words, remote_created_at, remote_updated_at, last_synced_at) VALUES
                (10, 8011, 1, 1, 101, 'first-report', 'Creating your first report', 'published',
                 'To create your first report, open the Reports section',
                 'To create your first report, open the Reports section and click New report. Pick a data source.', 320, 16,
                 '2026-04-10T10:30:00Z', '2026-09-25T10:30:00Z', '2026-10-06 10:00:00'),
                (11, 8012, 1, 1, 102, 'schedule-timezones', 'Understanding schedule timezones', 'published',
                 'Schedules store the UTC offset that was active',
                 'Schedules store the UTC offset that was active when you last saved them.', 540, 12,
                 '2026-05-09T10:30:00Z', '2026-10-04T10:30:00Z', '2026-10-06 10:00:00'),
                (12, 8021, 2, 2, 201, 'receipts', 'Downloading receipts and invoices', 'draft',
                 'DRAFT - not yet published. Every charge generates a receipt.',
                 'DRAFT - not yet published. Every charge generates a receipt you can download.', 45, 11,
                 '2026-09-26T10:30:00Z', '2026-10-05T10:30:00Z', '2026-10-06 10:05:00');
             INSERT INTO docs_fts (name, text, article_id) VALUES
                ('Creating your first report', 'To create your first report, open the Reports section and click New report. Pick a data source.', 10),
                ('Understanding schedule timezones', 'Schedules store the UTC offset that was active when you last saved them.', 11),
                ('Downloading receipts and invoices', 'DRAFT - not yet published. Every charge generates a receipt you can download.', 12);",
        )
        .unwrap();
    }

    fn params(pairs: &[(&str, &str)]) -> Query<HashMap<String, String>> {
        let mut map = HashMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_string(), (*v).to_string());
        }
        Query(map)
    }

    async fn body_json(response: impl IntoResponse) -> (StatusCode, Value) {
        let response = response.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    // ---- collections --------------------------------------------------------

    #[tokio::test]
    async fn collections_serve_the_mirror_shape_and_sync_flag() {
        let (state, conn) = make_state();
        seed_mirror(&conn.lock().unwrap());
        let (status, body) = body_json(collections(State(state)).await).await;
        assert_eq!(status, StatusCode::OK);
        let cols = body["collections"].as_array().unwrap();
        assert_eq!(cols.len(), 2);
        // ORDER BY name (Billing & Account first).
        assert_eq!(cols[0]["name"].as_str(), Some("Billing & Account"));
        assert_eq!(cols[0]["remote_id"].as_i64(), Some(802));
        assert_eq!(cols[0]["slug"].as_str(), Some("billing"));
        assert_eq!(cols[0]["description"].as_str(), Some("Plans and invoices"));
        assert_eq!(cols[0]["visibility"].as_str(), Some("public"));
        assert_eq!(cols[0]["article_count"].as_i64(), Some(1));
        assert!(cols[0]["last_synced_at"].as_str().is_some());
        assert_eq!(cols[0]["id"].as_i64(), Some(2));
        // Demo/fake provider -> honest capability flag.
        assert_eq!(body["docs_sync_available"].as_bool(), Some(true));
    }

    // ---- stats ----------------------------------------------------------------

    #[tokio::test]
    async fn stats_serve_the_reference_counters() {
        let (state, conn) = make_state();
        seed_mirror(&conn.lock().unwrap());
        let (status, body) = body_json(stats(State(state)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["collections"].as_i64(), Some(2));
        assert_eq!(body["articles"].as_i64(), Some(3));
        assert_eq!(body["published"].as_i64(), Some(2));
        assert_eq!(body["drafts"].as_i64(), Some(1));
        assert_eq!(body["internal"].as_i64(), Some(0));
        assert_eq!(body["total_views"].as_i64(), Some(320 + 540 + 45));
        assert!(body["last_synced_at"].as_str().is_some());
        assert_eq!(body["chat_sessions"].as_i64(), Some(0));
        assert_eq!(body["email_conversations"].as_i64(), Some(0));
        assert_eq!(body["docs_chunks"].as_i64(), Some(0));
        assert_eq!(body["docs_chunks_indexed"].as_i64(), Some(0));
    }

    // ---- search ----------------------------------------------------------------

    #[tokio::test]
    async fn search_validates_q_and_limit_like_the_reference() {
        let (state, _conn) = make_state();
        // Missing q.
        let (status, body) = body_json(search(State(state.clone()), params(&[])).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["statusCode"], 422);
        assert_eq!(body["error"], "ValidationError");
        assert_eq!(body["message"], "q is required.");
        // Blank q.
        let (status, body) =
            body_json(search(State(state.clone()), params(&[("q", "   ")])).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["message"], "q is required.");
        // limit bounds.
        for bad in ["0", "101", "3.5", "abc"] {
            let (status, body) = body_json(
                search(
                    State(state.clone()),
                    params(&[("q", "report"), ("limit", bad)]),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "limit={bad}");
            assert_eq!(body["message"], "limit must be 1-100.");
        }
        // Valid limit passes validation (q matches nothing).
        let (status, _) = body_json(
            search(
                State(state),
                params(&[("q", "zzznothing"), ("limit", "100")]),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn search_without_model_serves_fts_hits_with_honest_note() {
        let (state, conn) = make_state();
        seed_mirror(&conn.lock().unwrap());
        let (status, body) =
            body_json(search(State(state), params(&[("q", "report")])).await).await;
        assert_eq!(status, StatusCode::OK);
        // Only article 10 matches "report" (title + text).
        assert_eq!(body["total"].as_i64(), Some(1));
        let hit = &body["hits"][0];
        assert_eq!(hit["article"]["id"].as_i64(), Some(10));
        assert_eq!(
            hit["article"]["name"].as_str(),
            Some("Creating your first report")
        );
        assert_eq!(
            hit["article"]["collection_name"].as_str(),
            Some("Getting Started")
        );
        assert_eq!(hit["article"]["category_name"].as_str(), Some("Setup"));
        assert_eq!(hit["article"]["number"].as_i64(), Some(101));
        assert_eq!(hit["article"]["status"].as_str(), Some("published"));
        assert_eq!(hit["why"][0].as_str(), Some("fts"));
        // FTS snippet takes precedence over the preview.
        let snippet = hit["snippet"].as_str().unwrap();
        assert!(
            snippet.contains('[') && snippet.contains(']'),
            "snippet: {snippet}"
        );
        assert_eq!(hit["matched_chunk"].as_null(), Some(()));
        assert_eq!(body["used_semantic"].as_bool(), Some(false));
        assert_eq!(body["semantic_available"].as_bool(), Some(false));
        assert_eq!(body["mode_note"].as_str(), Some(MODE_NOTE_NO_MODEL));
        assert_eq!(body["query"].as_str(), Some("report"));
    }

    #[tokio::test]
    async fn search_semantic_can_be_disabled_per_query() {
        let (state, conn) = make_state();
        seed_mirror(&conn.lock().unwrap());
        let (status, body) =
            body_json(search(State(state), params(&[("q", "report"), ("semantic", "0")])).await)
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["used_semantic"].as_bool(), Some(false));
        assert_eq!(
            body["mode_note"].as_str(),
            Some(MODE_NOTE_SEMANTIC_DISABLED)
        );
        // semantic=true is the same as absent.
        let (state, _conn) = make_state();
        let (_, body) =
            body_json(search(State(state), params(&[("q", "x"), ("semantic", "true")])).await)
                .await;
        assert_eq!(
            body["mode_note"].as_str(),
            Some(MODE_NOTE_NO_MODEL),
            "semantic=true must behave like absent"
        );
    }

    /// A fake LM Studio serving /v1/embeddings — every input embeds to
    /// `query_vector`.
    fn spawn_fake_lm_studio(query_vector: Vec<f32>) -> String {
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| async move {
                let inputs = body["input"].as_array().map(Vec::len).unwrap_or(0);
                let data: Vec<Value> = (0..inputs)
                    .map(|_| serde_json::json!({ "embedding": query_vector }))
                    .collect();
                axum::Json(serde_json::json!({ "data": data }))
            }),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        std::thread::spawn(move || {
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let _ = axum::serve(listener, app).await;
            });
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn search_hybrid_uses_local_cosine_with_matched_chunk() {
        let (state, conn) = make_state();
        {
            let conn = conn.lock().unwrap();
            seed_mirror(&conn);
            // Configure a model + a fake LM Studio (query embeds to [1.0, 0.0]).
            let base = spawn_fake_lm_studio(vec![1.0, 0.0]);
            crate::settings::set_string(&conn, "lmstudio_base_url", &base).unwrap();
            crate::settings::set_string(&conn, "lmstudio_embedding_model", "test-embed").unwrap();
            crate::settings::set_i64(&conn, "lmstudio_timeout_ms", 2_000).unwrap();
            // One indexed chunk on article 11 whose embedding points the
            // same direction as the query vector.
            conn.execute(
                "INSERT INTO docs_chunks (article_id, chunk_index, content, embedding, embedding_model, embedding_state)
                 VALUES (11, 0, 'Schedules store the UTC offset that was active when you last saved them.',
                         ?1, 'test-embed', 'indexed')",
                rusqlite::params![crate::embeddings::encode_f32_le(&[1.0, 0.0])],
            )
            .unwrap();
        }
        let (status, body) =
            body_json(search(State(state), params(&[("q", "schedule")])).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["used_semantic"].as_bool(), Some(true));
        assert_eq!(body["semantic_available"].as_bool(), Some(true));
        // Qdrant is disabled -> local cosine scan note.
        assert_eq!(body["mode_note"].as_str(), Some(MODE_NOTE_LOCAL_COSINE));
        // Article 11 (FTS + semantic) outranks article 10 (FTS only) and the
        // semantic-only hit carries the matched chunk preview.
        let hits = body["hits"].as_array().unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0]["article"]["id"].as_i64(), Some(11));
        assert!(hits[0]["why"]
            .as_array()
            .unwrap()
            .contains(&json!("semantic")));
        let matched = hits[0]["matched_chunk"].as_str().unwrap();
        assert!(matched.starts_with("Schedules store the UTC offset"));
        // RRF-fused score is present on every hit.
        assert!(hits[0]["score"].as_f64().is_some());
    }

    #[tokio::test]
    async fn search_reports_no_chunks_note_when_model_set_but_nothing_embedded() {
        let (state, conn) = make_state();
        {
            let conn = conn.lock().unwrap();
            seed_mirror(&conn);
            crate::settings::set_string(&conn, "lmstudio_embedding_model", "test-embed").unwrap();
        }
        let (status, body) =
            body_json(search(State(state), params(&[("q", "report")])).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["mode_note"].as_str(), Some(MODE_NOTE_NO_CHUNKS));
        assert_eq!(body["semantic_available"].as_bool(), Some(false));
        // FTS still serves the hit.
        assert_eq!(body["total"].as_i64(), Some(1));
    }

    // ---- articles --------------------------------------------------------------

    #[tokio::test]
    async fn articles_are_paged_with_filters_and_validation() {
        let (state, conn) = make_state();
        seed_mirror(&conn.lock().unwrap());
        // Defaults: page 1, pageSize 25 -> everything.
        let (status, body) = body_json(articles(State(state.clone()), params(&[])).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"].as_i64(), Some(3));
        assert_eq!(body["page"].as_i64(), Some(1));
        assert_eq!(body["page_size"].as_i64(), Some(25));
        let list = body["articles"].as_array().unwrap();
        assert_eq!(list.len(), 3);
        // ORDER BY COALESCE(remote_updated_at, remote_created_at) DESC.
        assert_eq!(list[0]["id"].as_i64(), Some(12));
        assert_eq!(list[1]["id"].as_i64(), Some(11));
        assert_eq!(list[2]["id"].as_i64(), Some(10));
        assert_eq!(list[0]["status"].as_str(), Some("draft"));

        // Paging.
        let (_, page2) = body_json(
            articles(
                State(state.clone()),
                params(&[("page", "2"), ("pageSize", "2")]),
            )
            .await,
        )
        .await;
        assert_eq!(page2["total"].as_i64(), Some(3));
        assert_eq!(page2["articles"].as_array().unwrap().len(), 1);
        assert_eq!(page2["page"].as_i64(), Some(2));
        assert_eq!(page2["page_size"].as_i64(), Some(2));

        // collectionId filter.
        let (_, by_col) =
            body_json(articles(State(state.clone()), params(&[("collectionId", "2")])).await).await;
        assert_eq!(by_col["total"].as_i64(), Some(1));
        assert_eq!(by_col["articles"][0]["collection_id"].as_i64(), Some(2));

        // status filter.
        let (_, by_status) =
            body_json(articles(State(state.clone()), params(&[("status", "draft")])).await).await;
        assert_eq!(by_status["total"].as_i64(), Some(1));
        assert_eq!(by_status["articles"][0]["id"].as_i64(), Some(12));

        // q filter (FTS).
        let (_, by_q) =
            body_json(articles(State(state.clone()), params(&[("q", "timezone")])).await).await;
        assert_eq!(by_q["total"].as_i64(), Some(1));
        assert_eq!(by_q["articles"][0]["id"].as_i64(), Some(11));

        // Validation envelopes.
        for bad in [
            vec![("page", "0")],
            vec![("pageSize", "101")],
            vec![("page", "x")],
        ] {
            let (status, body) =
                body_json(articles(State(state.clone()), params(&bad)).await).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "bad={bad:?}");
            assert_eq!(
                body["message"],
                "page must be >= 1 and pageSize must be 1-100."
            );
        }
        let (status, body) =
            body_json(articles(State(state.clone()), params(&[("collectionId", "-3")])).await)
                .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["message"], "collectionId must be a positive integer.");
        let (status, body) =
            body_json(articles(State(state.clone()), params(&[("status", "archived")])).await)
                .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            "status must be one of 'published', 'draft', 'internal'."
        );
        // Blank collectionId/status behave as absent.
        let (status, body) = body_json(
            articles(
                State(state),
                params(&[("collectionId", ""), ("status", "")]),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"].as_i64(), Some(3));
    }

    // ---- article ----------------------------------------------------------------

    #[tokio::test]
    async fn article_detail_serves_text_and_404_envelope() {
        let (state, conn) = make_state();
        seed_mirror(&conn.lock().unwrap());
        let (status, body) = article_detail(State(state.clone()), 10).await;
        assert_eq!(status, StatusCode::OK);
        let article = &body["article"];
        assert_eq!(article["id"].as_i64(), Some(10));
        assert_eq!(article["remote_id"].as_i64(), Some(8011));
        assert_eq!(
            article["text"].as_str(),
            Some("To create your first report, open the Reports section and click New report. Pick a data source.")
        );
        assert_eq!(article["words"].as_i64(), Some(16));
        // Unknown id -> the reference 404 envelope.
        let (status, body) = article_detail(State(state.clone()), 999).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["statusCode"], 404);
        assert_eq!(body["error"], "NotFound");
        assert_eq!(
            body["message"],
            "Article not found in the local docs mirror. Run a sync with a Docs API key configured."
        );
        // Non-positive id -> 422.
        let (status, body) = article_detail(State(state), 0).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["message"], "Article id must be a positive integer.");
    }

    /// Direct handler call wrapper (Path extraction is skipped in unit
    /// tests; the integration suite exercises the route through real HTTP).
    async fn article_detail(state: State<AppState>, id: i64) -> (StatusCode, Value) {
        body_json(article(state, Path(id)).await).await
    }
}
