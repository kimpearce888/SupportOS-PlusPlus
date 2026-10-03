//! Search routes — mirrors src/server/routes/search.ts (hybrid v1.5.0).
//!
//! POST /api/search is the universal search endpoint:
//! - Retriever 1 is always the FTS5 engine (`crate::search::search`) —
//!   zero configuration, handles the empty query (recent conversations).
//! - Retriever 2 (tickets only) is semantic: the query embedding against
//!   `conversation_chunks`. The port has no Qdrant client, so the LOCAL
//!   cosine scan over the stored Float32 embeddings is the always-available
//!   path (semantic ticket search is not hostage to a vector db).
//! - Fusion is Reciprocal Rank Fusion (`hybrid_search::merge_doc_hits`,
//!   k = 60): FTS ranks and cosine scores live on incommensurable scales,
//!   so only RANKS are fused. Provenance is preserved per hit.
//! - Semantic is skipped honestly when no embedding model is configured or
//!   no chunks are indexed yet — `mode_note` says which mode produced the
//!   hits (`used_semantic` / `semantic_available` report the rest).
//!
//! Validation mirrors the reference `searchRequestSchema` (zod) + Fastify:
//! malformed JSON → 400; schema violations → 422 ValidationError with the
//! first issue in the message (reference `app.ts` error handler).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use rusqlite::Connection;
use serde_json::{json, Value};

use super::super::server::AppState;
use crate::embeddings::{self, utf16_len};
use crate::hybrid_search::{cosine_similarity, merge_doc_hits, FtsDocHit, SemanticDocHit};
use crate::search::{SearchFilters, SearchHit, SearchResponse};

/// Reference `z.string().max(500)` on `query`.
const MAX_QUERY_UNITS: usize = 500;

/// How many conversations the local cosine scan ranks (reference: 24).
const SEMANTIC_TICKET_LIMIT: usize = 24;

/// Fused ticket hits kept after RRF (reference: `mergeDocHits(..., 40)`).
const MERGED_TICKET_LIMIT: usize = 40;

// Reference mode_note strings (exact).
/// Qdrant ANN served the semantic retriever (kept for parity; the port has
/// no Qdrant client, so this variant is currently unreachable).
const MODE_NOTE_QDRANT: &str =
    "Hybrid ticket search: FTS5 + semantic vectors via Qdrant, fused with Reciprocal Rank Fusion.";
/// Qdrant enabled but did not serve — the local cosine scan ran instead.
const MODE_NOTE_LOCAL_QDRANT_DOWN: &str = "Hybrid ticket search: FTS5 + semantic vectors (local cosine scan - Qdrant not reachable), fused with Reciprocal Rank Fusion.";
/// Qdrant disabled — local semantic vectors only.
const MODE_NOTE_LOCAL: &str =
    "Hybrid ticket search: FTS5 + local semantic vectors, fused with Reciprocal Rank Fusion.";
/// Model configured, nothing embedded yet.
const MODE_NOTE_NO_CHUNKS: &str = "Keyword search (FTS5). An embedding model is configured but no ticket chunks are embedded yet - run a sync so the embedding job can process the mirror.";
/// No model configured.
const MODE_NOTE_NO_MODEL: &str = "Keyword search (FTS5). Semantic ticket search needs an embedding model: Settings → LM Studio → embedding model, then the ticket embedding job runs on the next sync.";

/// POST /api/search — hybrid universal search (FTS5 + semantic tickets).
pub async fn search(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    // Fastify parses the JSON body before the handler; malformed JSON is a
    // 400 Bad Request (port convention: the webhook route's message).
    let body: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "statusCode": 400,
                    "error": "Bad Request",
                    "message": "Request body is not valid JSON."
                })),
            );
        }
    };

    // searchRequestSchema.parse — collect zod-shaped issues.
    let issues = validate_search_request(&body);
    if !issues.is_empty() {
        let first = &issues[0];
        let where_hint = if first.0.is_empty() {
            String::new()
        } else {
            format!(" ({})", first.0)
        };
        let issue_list: Vec<Value> = issues
            .iter()
            .take(10)
            .map(|(path, message)| json!({ "path": path, "message": message }))
            .collect();
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": format!("Invalid request{where_hint}: {}", first.1),
                "issues": issue_list
            })),
        );
    }

    // Defaults: query '' and scope 'all' (zod .default(...)).
    let query = body
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let scope = body
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("all")
        .to_string();
    let filters = parse_filters(&body);

    // Everything that needs the connection guard happens in this block; it
    // ends before the embed await (a MutexGuard held across an await makes
    // the handler future !Send).
    let (mut fts, settings, qdrant_enabled, stats) = {
        let conn = state.conn_lock();
        let fts = match crate::search::search(&conn, &query, &scope, &filters) {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message": e.to_string()})),
                )
            }
        };
        // Hybrid layer — reference search.ts: settings + one memoized query
        // embedding shared by the retrievers (v1.6.0 audit fix: embed at
        // most once per request; the port's only consumer is the ticket
        // retriever, so a single call below satisfies the memoization).
        let settings = match embeddings::lm_studio_embedding_settings(&conn) {
            Ok(s) => s,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message": e.to_string()})),
                )
            }
        };
        // The reference defaults `qdrant_enabled` to true; the embedded
        // adapter (D2) answers health/search in-process, so the same default
        // is honest here.
        let qdrant_enabled =
            crate::settings::get_bool(&conn, "qdrant_enabled", true).unwrap_or(true);
        let stats = embeddings::conversation_chunk_stats(&conn);
        (fts, settings, qdrant_enabled, stats)
    };

    if !query.trim().is_empty() && (scope == "all" || scope == "tickets") {
        let stats = match stats {
            Ok(s) => s,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message": e.to_string()})),
                )
            }
        };
        if settings.embedding_model.is_some() && stats.indexed > 0 {
            // Reference embedQuery(): gated on model + non-empty query; an
            // embed failure yields null (caught) and the hybrid layer is
            // silently skipped. This is the single embed call per request.
            // The connection guard is released for the await (the embed is
            // pure HTTP) and re-acquired for the local retriever.
            let ticket_query_vector: Option<Vec<f32>> = match &settings.embedding_model {
                Some(model) => {
                    match embeddings::embed_texts(
                        &settings.base_url,
                        model,
                        &[query.clone()],
                        settings.timeout_ms,
                    )
                    .await
                    {
                        Ok(vectors) => vectors.into_iter().next().filter(|v| !v.is_empty()),
                        Err(_) => None,
                    }
                }
                None => None,
            };

            if let Some(vector) = ticket_query_vector.filter(|v| !v.is_empty()) {
                // Reference: Qdrant ANN first (24 hits, entity-filtered,
                // first-wins grouping per conversation), local cosine scan
                // fallback when it serves nothing.
                let mut semantic: Vec<SemanticTicketHit> = Vec::new();
                let mut served_by_qdrant = false;
                if qdrant_enabled {
                    let hits = state.qdrant.search(&vector, SEMANTIC_TICKET_LIMIT);
                    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
                    for h in &hits {
                        if h.payload.get("entity_type").and_then(|v| v.as_str())
                            != Some("conversation_chunk")
                        {
                            continue;
                        }
                        let Some(conv_id) = h.payload.get("entity_id").and_then(|v| v.as_i64())
                        else {
                            continue;
                        };
                        if seen.insert(conv_id) {
                            let snippet = h
                                .payload
                                .get("text")
                                .and_then(|v| v.as_str())
                                .map(|t| embeddings::utf16_slice(t, 0, 200));
                            semantic.push(SemanticTicketHit {
                                conversation_id: conv_id,
                                rank: semantic.len(),
                                snippet,
                            });
                        }
                    }
                    served_by_qdrant = !semantic.is_empty();
                }
                if semantic.is_empty() {
                    // Local cosine scan over stored embeddings (the reference's
                    // no-Qdrant path — identical ranks and snippets).
                    let conn = state.conn_lock();
                    match semantic_ticket_retrieval(&conn, &vector) {
                        Ok(local) => semantic = local,
                        Err(_) => {
                            // Retriever failure: keyword search remains fully
                            // functional (reference catch block).
                            fts.semantic_available = false;
                            fts.total = fts.hits.len();
                            return (
                                StatusCode::OK,
                                Json(serde_json::to_value(&fts).unwrap_or(Value::Null)),
                            );
                        }
                    }
                }
                let mode_note = if served_by_qdrant {
                    MODE_NOTE_QDRANT
                } else if qdrant_enabled {
                    MODE_NOTE_LOCAL_QDRANT_DOWN
                } else {
                    MODE_NOTE_LOCAL
                };
                let conn = state.conn_lock();
                rebuild_ticket_hits(&conn, &mut fts, &semantic);
                fts.used_semantic = true;
                fts.semantic_available = true;
                fts.mode_note = Some(mode_note.to_string());

                // Knowledge semantic layer (reference search.ts:137-161):
                // Qdrant-only, adds knowledge hits the keyword pass missed.
                if settings.embedding_model.is_some() && qdrant_enabled {
                    let hits = state.qdrant.search(&vector, 8);
                    for h in &hits {
                        if h.payload.get("entity_type").and_then(|v| v.as_str())
                            != Some("knowledge_chunk")
                        {
                            continue;
                        }
                        let (Some(entity_id), Some(title), Some(text)) = (
                            h.payload.get("entity_id").and_then(|v| v.as_i64()),
                            h.payload.get("title").and_then(|v| v.as_str()),
                            h.payload.get("text").and_then(|v| v.as_str()),
                        ) else {
                            continue;
                        };
                        let already = fts
                            .hits
                            .iter()
                            .any(|x| x.scope == "knowledge" && x.id == entity_id);
                        if already {
                            continue;
                        }
                        let visibility = if h.payload.get("visibility").and_then(|v| v.as_str())
                            == Some("customer_safe")
                        {
                            "customer-safe"
                        } else {
                            "internal"
                        };
                        fts.hits.push(crate::search::SearchHit {
                            scope: "knowledge".to_string(),
                            id: entity_id,
                            title: title.to_string(),
                            subtitle: format!("Knowledge · {visibility}"),
                            snippet: embeddings::utf16_slice(text, 0, 200),
                            score: h.score as f64,
                            href: format!("/knowledge/{entity_id}"),
                            why: vec!["semantic match".to_string()],
                        });
                    }
                    fts.total = fts.hits.len();
                }
            }
        } else if settings.embedding_model.is_some() && stats.indexed == 0 {
            fts.mode_note = Some(MODE_NOTE_NO_CHUNKS.to_string());
        } else if settings.embedding_model.is_none() {
            fts.mode_note = Some(MODE_NOTE_NO_MODEL.to_string());
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::to_value(&fts).unwrap_or(Value::Null)),
    )
}

/// Is `scope` one of the seven reference values?
fn is_valid_scope(scope: &str) -> bool {
    matches!(
        scope,
        "all" | "tickets" | "customers" | "knowledge" | "issues" | "saved_replies" | "ai"
    )
}

/// Zod's name for a JSON value's type ("Expected object, received array").
fn zod_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Zod's "received" rendering inside enum messages (strings quoted).
fn zod_received(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "null".into(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
    }
}

/// Validate the request body against the reference `searchRequestSchema`
/// (zod v3 semantics): `query` string ≤ 500 UTF-16 units defaulting to '',
/// `scope` one of the seven values defaulting to 'all', `filters` an object
/// with optional typed fields defaulting to {}. Unknown keys are stripped
/// (allowed). Returns `(path, message)` issues in schema order.
fn validate_search_request(body: &Value) -> Vec<(String, String)> {
    let mut issues: Vec<(String, String)> = Vec::new();
    let Some(obj) = body.as_object() else {
        issues.push((
            String::new(),
            format!("Expected object, received {}", zod_type_name(body)),
        ));
        return issues;
    };

    // query: z.string().max(500).default('')
    if let Some(q) = obj.get("query") {
        if !q.is_string() {
            issues.push((
                "query".into(),
                format!("Expected string, received {}", zod_type_name(q)),
            ));
        } else if utf16_len(q.as_str().unwrap_or("")) > MAX_QUERY_UNITS {
            issues.push((
                "query".into(),
                "String must contain at most 500 character(s)".into(),
            ));
        }
    }

    // scope: z.enum([...]).default('all')
    if let Some(s) = obj.get("scope") {
        if !s.as_str().is_some_and(is_valid_scope) {
            issues.push((
                "scope".into(),
                format!(
                    "Invalid enum value. Expected 'all' | 'tickets' | 'customers' | 'knowledge' | 'issues' | 'saved_replies' | 'ai', received {}",
                    zod_received(s)
                ),
            ));
        }
    }

    // filters: object with optional typed fields, default {}
    if let Some(f) = obj.get("filters") {
        if !f.is_object() {
            issues.push((
                "filters".into(),
                format!("Expected object, received {}", zod_type_name(f)),
            ));
        } else {
            let f = f.as_object().expect("checked above");
            // Reference schema field order: status, mailbox_id, tag,
            // since_days, assignee_id (zod reports issues in that order).
            for (key, expected) in [
                ("status", "string"),
                ("mailbox_id", "number"),
                ("tag", "string"),
                ("since_days", "number"),
                ("assignee_id", "number"),
            ] {
                if let Some(v) = f.get(key) {
                    let matches = if expected == "string" {
                        v.is_string()
                    } else {
                        v.is_number()
                    };
                    if !matches {
                        issues.push((
                            format!("filters.{key}"),
                            format!("Expected {expected}, received {}", zod_type_name(v)),
                        ));
                    }
                }
            }
        }
    }

    issues
}

/// Extract `filters` into the engine's filter struct (post-validation —
/// every present field already has the right type).
fn parse_filters(body: &Value) -> SearchFilters {
    let empty = serde_json::Map::new();
    let f = body
        .get("filters")
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    let get_str = |k: &str| f.get(k).and_then(|v| v.as_str()).map(String::from);
    let get_num = |k: &str| f.get(k).and_then(|v| v.as_i64());
    SearchFilters {
        status: get_str("status"),
        mailbox_id: get_num("mailbox_id"),
        tag: get_str("tag"),
        since_days: get_num("since_days"),
        assignee_id: get_num("assignee_id"),
    }
}

/// A conversation ranked by the semantic retriever, with the best chunk's
/// snippet (reference `semanticByConversation` map entry).
struct SemanticTicketHit {
    conversation_id: i64,
    /// Similarity rank among the top [`SEMANTIC_TICKET_LIMIT`].
    rank: usize,
    /// Best-matching chunk content, capped at 200 UTF-16 units.
    snippet: Option<String>,
}

/// The semantic ticket retriever: local cosine scan over the stored Float32
/// embeddings (reference Qdrant-else-local branch), returning the top
/// [`SEMANTIC_TICKET_LIMIT`] conversations in similarity order.
///
/// # Errors
///
/// Returns `Error::Sqlite` when the chunk scan fails (the route then keeps
/// keyword-only results, mirroring the reference catch block).
fn semantic_ticket_retrieval(
    conn: &Connection,
    query_vector: &[f32],
) -> crate::error::Result<Vec<SemanticTicketHit>> {
    let chunks = embeddings::list_conversation_chunks_with_embedding(conn)?;
    // Insertion-ordered best-per-conversation map (reference Map semantics:
    // the first chunk sets the score, later chunks replace on strict >).
    let mut best: Vec<(i64, f32, String)> = Vec::new();
    for chunk in chunks {
        let sim = cosine_similarity(query_vector, &chunk.embedding);
        match best
            .iter_mut()
            .find(|(cid, _, _)| *cid == chunk.conversation_id)
        {
            Some(entry) => {
                if sim > entry.1 {
                    entry.1 = sim;
                    entry.2 = chunk.content;
                }
            }
            None => best.push((chunk.conversation_id, sim, chunk.content)),
        }
    }
    // Sort by similarity descending (stable, like JS Array.sort) and cap.
    best.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    best.truncate(SEMANTIC_TICKET_LIMIT);

    Ok(best
        .into_iter()
        .enumerate()
        .map(|(rank, (conversation_id, _, content))| SemanticTicketHit {
            conversation_id,
            rank,
            snippet: Some(embeddings::utf16_slice(&content, 0, 200)),
        })
        .collect())
}

/// Rebuild the tickets hit list from the RRF fusion (reference merge +
/// rebuild): FTS hits keep their snippets (score replaced by the fused
/// score, 'semantic match' appended to `why` when the semantic retriever
/// also found them); semantic-only hits get chunk snippets + provenance.
/// Non-ticket hits are appended unchanged.
fn rebuild_ticket_hits(
    conn: &Connection,
    fts: &mut SearchResponse,
    semantic: &[SemanticTicketHit],
) {
    let fts_tickets: Vec<SearchHit> = fts
        .hits
        .iter()
        .filter(|h| h.scope == "tickets")
        .cloned()
        .collect();
    let fts_ranks: Vec<FtsDocHit> = fts_tickets
        .iter()
        .enumerate()
        .map(|(i, h)| FtsDocHit {
            article_id: h.id,
            rank: i,
        })
        .collect();
    // The semantic list is already in similarity order (its rank IS the
    // signal); the score is kept for shape parity with the reference.
    let semantic_ranks: Vec<SemanticDocHit> = semantic
        .iter()
        .map(|s| SemanticDocHit {
            article_id: s.conversation_id,
            score: 1.0 / (1.0 + s.rank as f32),
        })
        .collect();
    let merged = merge_doc_hits(&fts_ranks, &semantic_ranks, MERGED_TICKET_LIMIT);

    let fts_by_id: std::collections::HashMap<i64, &SearchHit> =
        fts_tickets.iter().map(|h| (h.id, h)).collect();

    let mut rebuilt: Vec<SearchHit> = Vec::new();
    for m in &merged {
        if let Some(existing) = fts_by_id.get(&m.article_id) {
            let mut hit = (*existing).clone();
            hit.score = m.score;
            if m.why.iter().any(|w| *w == "semantic") {
                hit.why.push("semantic match".into());
            }
            rebuilt.push(hit);
            continue;
        }
        // Semantic-only hit: rebuild from the conversation row (missing
        // conversations are skipped, like the reference).
        let conv: Option<(i64, i64, Option<String>)> = conn
            .query_row(
                "SELECT id, number, subject FROM conversations WHERE id = ?1",
                rusqlite::params![m.article_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        let Some((id, number, subject)) = conv else {
            continue;
        };
        let snippet = semantic
            .iter()
            .find(|s| s.conversation_id == m.article_id)
            .and_then(|s| s.snippet.clone())
            .unwrap_or_default();
        rebuilt.push(SearchHit {
            scope: "tickets".into(),
            id,
            title: format!(
                "#{} {}",
                number,
                subject.unwrap_or_else(|| "(no subject)".into())
            ),
            subtitle: "semantic match".into(),
            snippet,
            score: m.score,
            href: format!("/inbox/conversation/{id}"),
            why: vec!["semantic match".into()],
        });
    }

    let mut hits: Vec<SearchHit> = rebuilt;
    hits.extend(fts.hits.drain(..).filter(|h| h.scope != "tickets"));
    fts.hits = hits;
    fts.total = fts.hits.len();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::Response;
    use rusqlite::params;
    use std::sync::{Arc, Mutex};

    fn fresh_db() -> Connection {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        crate::embeddings::apply_m008(&conn).unwrap();
        crate::ai_center::apply_m009(&conn).unwrap();
        crate::inbox::apply_m028(&conn).unwrap();
        crate::conversation_ops::apply_m030(&conn).unwrap();
        crate::intelligence_features::apply_m015_to_m019(&conn).unwrap();
        crate::sync_schema::apply_m029(&conn).unwrap();
        crate::customer_events::apply_m036(&conn).unwrap();
        crate::mirror_tables::apply_m039(&conn).unwrap();
        crate::search::apply_fts_migration(&conn).unwrap();
        conn
    }

    fn make_state() -> (AppState, Arc<Mutex<Connection>>) {
        let conn = fresh_db();
        // Qdrant disabled in settings AND adapter disabled — a coherent
        // state (production constructs both from the same setting).
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

    async fn body_json(response: impl IntoResponse) -> (StatusCode, Value) {
        let response = response.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    fn bytes(v: &Value) -> Bytes {
        Bytes::from(serde_json::to_vec(v).unwrap())
    }

    fn insert_conversation(conn: &Connection, id: i64, subject: &str) {
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, preview, mailbox_id, customer_id)
             VALUES (?1, ?1, ?2, ?3, ?4, 1, 1)",
            params![id, 100 + id, subject, format!("preview of {}", subject.to_lowercase())],
        )
        .unwrap();
        // The reference indexes on write (repo delete+insert); tests must
        // exercise the same FTS population.
        crate::search::index_conversation_fts(conn, id).unwrap();
    }

    /// Insert an indexed chunk for `conversation_id` whose content embeds to
    /// `vector`.
    fn insert_indexed_chunk(
        conn: &Connection,
        conversation_id: i64,
        content: &str,
        vector: &[f32],
    ) {
        conn.execute(
            "INSERT INTO conversation_chunks (conversation_id, chunk_index, content, embedding, embedding_model, embedding_state)
             VALUES (?1, 0, ?2, ?3, 'test-embed', 'indexed')",
            params![conversation_id, content, crate::embeddings::encode_f32_le(vector)],
        )
        .unwrap();
    }

    /// A fake LM Studio serving `/v1/embeddings` — the query text embeds to
    /// `query_vector`, everything else to zeros.
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

    fn configure_lm_studio(conn: &Connection, base_url: &str) {
        crate::settings::set_string(conn, "lmstudio_base_url", base_url).unwrap();
        crate::settings::set_string(conn, "lmstudio_embedding_model", "test-embed").unwrap();
        crate::settings::set_i64(conn, "lmstudio_timeout_ms", 2_000).unwrap();
    }

    // ---- validation (reference zod + Fastify semantics) --------------------

    #[tokio::test]
    async fn non_string_query_is_422_validation_error() {
        let (state, _conn) = make_state();
        let (status, body) =
            body_json(search(State(state), bytes(&serde_json::json!({ "query": 123 }))).await)
                .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["statusCode"], 422);
        assert_eq!(body["error"], "ValidationError");
        assert!(
            body["message"].as_str().unwrap().contains("query"),
            "message: {body}"
        );
        assert_eq!(
            body["message"],
            "Invalid request (query): Expected string, received number"
        );
        assert_eq!(body["issues"][0]["path"], "query");
    }

    #[tokio::test]
    async fn oversized_query_is_422() {
        let (state, _conn) = make_state();
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "A".repeat(2_000_000) })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            "Invalid request (query): String must contain at most 500 character(s)"
        );
    }

    #[tokio::test]
    async fn invalid_scope_is_422() {
        let (state, _conn) = make_state();
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "x", "scope": "bogus" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let msg = body["message"].as_str().unwrap();
        assert!(
            msg.starts_with("Invalid request (scope): Invalid enum value. Expected 'all'"),
            "{msg}"
        );
        assert!(msg.contains("received 'bogus'"), "{msg}");
    }

    #[tokio::test]
    async fn malformed_filters_are_422_not_500() {
        // Reference audit-phase1: hostile filters must not 500.
        let (state, _conn) = make_state();
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({
                    "query": "timezone",
                    "filters": { "mailbox_id": "inject", "since_days": "DROP", "tag": null }
                })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            "Invalid request (filters.mailbox_id): Expected number, received string"
        );
        // All three issues are reported (zod collects, then caps at 10).
        assert_eq!(body["issues"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn non_object_body_is_422() {
        let (state, _conn) = make_state();
        let (status, body) =
            body_json(search(State(state), bytes(&serde_json::json!([1, 2]))).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            "Invalid request: Expected object, received array"
        );
    }

    #[tokio::test]
    async fn malformed_json_is_400() {
        let (state, _conn) = make_state();
        let (status, body) =
            body_json(search(State(state), Bytes::from_static(b"{not json")).await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["statusCode"], 400);
        assert_eq!(body["error"], "Bad Request");
        assert_eq!(body["message"], "Request body is not valid JSON.");
    }

    // ---- honest mode reporting (reference v1.5 e2e) ------------------------

    #[tokio::test]
    async fn without_model_reports_keyword_mode_honestly() {
        let (state, conn) = make_state();
        insert_conversation(&conn.lock().unwrap(), 1, "Timezone report");
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "timezone report", "scope": "all", "filters": {} })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // Keyword search still works: the FTS hit surfaces with keyword
        // provenance; semantic is honestly reported as unavailable.
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "keyword hit: {hits:?}");
        assert_eq!(hits[0]["id"], 1);
        assert_eq!(hits[0]["why"], serde_json::json!(["keyword match"]));
        assert_eq!(body["semantic_available"], false);
        assert_eq!(body["used_semantic"], false);
        let note = body["mode_note"].as_str().unwrap();
        assert!(note.contains("embedding model"), "note: {note}");
        assert_eq!(note, MODE_NOTE_NO_MODEL);
    }

    #[tokio::test]
    async fn model_without_chunks_reports_pending_embedding_job() {
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            insert_conversation(&c, 1, "Timezone report");
            configure_lm_studio(&c, "http://127.0.0.1:9");
        }
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "timezone", "scope": "all" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["used_semantic"], false);
        assert_eq!(body["mode_note"], MODE_NOTE_NO_CHUNKS);
    }

    #[tokio::test]
    async fn unreachable_lm_studio_leaves_keyword_results() {
        // Model + indexed chunks, but the embed endpoint is down: the
        // reference's embedQuery catches -> null -> hybrid silently skipped.
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            insert_conversation(&c, 1, "Timezone report");
            insert_indexed_chunk(&c, 1, "chunk text", &[0.1, 0.2, 0.3]);
            configure_lm_studio(&c, "http://127.0.0.1:9"); // connection refused
        }
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "timezone", "scope": "all" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["used_semantic"], false);
        assert_eq!(body["semantic_available"], false);
        assert!(body.get("mode_note").is_none(), "no note on embed failure");
    }

    #[tokio::test]
    async fn empty_query_returns_recent_conversations_without_mode_note() {
        let (state, conn) = make_state();
        insert_conversation(&conn.lock().unwrap(), 1, "Anything");
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "", "scope": "all" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["query"], "");
        // Reference: an empty query with no filters returns RECENT
        // conversations (score 0.5, why ['recent']) — not an empty list.
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "recent conversation returned: {hits:?}");
        assert_eq!(hits[0]["id"], 1);
        assert_eq!(hits[0]["why"], serde_json::json!(["recent"]));
        assert_eq!(body["used_semantic"], false);
        assert!(
            body.get("mode_note").is_none(),
            "empty query skips the hybrid block"
        );
    }

    // ---- hybrid ticket retrieval (RRF + provenance) -------------------------

    #[tokio::test]
    async fn hybrid_search_fuses_and_labels_provenance() {
        let query_vector = vec![1.0_f32, 0.0, 0.0];
        let base_url = spawn_fake_lm_studio(query_vector.clone());
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            configure_lm_studio(&c, &base_url);
            insert_conversation(&c, 1, "Billing refund question");
            insert_conversation(&c, 2, "Unrelated subject");
            // Conv 1 has an indexed chunk aligned with the query vector.
            insert_indexed_chunk(&c, 1, "refund processing takes too long", &[1.0, 0.0, 0.0]);
            // Conv 2's chunk is orthogonal to the query vector.
            insert_indexed_chunk(&c, 2, "totally different topic", &[0.0, 1.0, 0.0]);
        }

        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "refund", "scope": "all" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["used_semantic"], true);
        assert_eq!(body["semantic_available"], true);
        assert_eq!(body["mode_note"], MODE_NOTE_LOCAL);

        let hits = body["hits"].as_array().unwrap();
        assert!(!hits.is_empty(), "hybrid hits must surface");
        let conv1 = hits
            .iter()
            .find(|h| h["id"] == 1)
            .expect("conversation 1 surfaced via FTS + semantic");
        // Conv 1 matches the FTS query ("refund" in the subject) AND ranks
        // first semantically: the reference rebuild keeps the FTS hit's
        // shape and appends 'semantic match' to why (provenance 'both').
        assert_eq!(conv1["scope"], "tickets");
        assert_eq!(conv1["title"], "#101 Billing refund question");
        assert_eq!(conv1["href"], "/inbox/conversation/1");
        assert_eq!(
            conv1["why"],
            serde_json::json!(["keyword match", "semantic match"])
        );
        // RRF: FTS rank 0 + semantic rank 0 = 2/(60+0+1) = 0.0328.
        assert_eq!(conv1["score"], 0.0328);
        // The reference's local cosine scan has NO score threshold (top 24
        // by similarity), so the orthogonal conversation still surfaces as a
        // lower-ranked semantic-only hit — honest RRF behavior.
        let conv2 = hits
            .iter()
            .find(|h| h["id"] == 2)
            .expect("no-threshold local scan surfaces conv 2 too");
        assert_eq!(conv2["subtitle"], "semantic match");
        assert_eq!(conv2["why"], serde_json::json!(["semantic match"]));
        // Conv 1 (both retrievers) outranks conv 2 (semantic only).
        let pos1 = hits.iter().position(|h| h["id"] == 1).unwrap();
        let pos2 = hits.iter().position(|h| h["id"] == 2).unwrap();
        assert!(pos1 < pos2, "hits: {hits:?}");
        assert_eq!(body["total"], body["hits"].as_array().unwrap().len());
    }

    #[tokio::test]
    async fn hybrid_search_keeps_fts_snippets_and_appends_semantic_match() {
        let query_vector = vec![1.0_f32, 0.0];
        let base_url = spawn_fake_lm_studio(query_vector.clone());
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            configure_lm_studio(&c, &base_url);
            // A conversation the FTS engine will find via its subject.
            insert_conversation(&c, 7, "Timezone report");
            insert_indexed_chunk(&c, 7, "timezone contents", &[1.0, 0.0]);
        }
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "Timezone report", "scope": "tickets" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["used_semantic"], true);
        let hits = body["hits"].as_array().unwrap();
        let hit = hits.iter().find(|h| h["id"] == 7).expect("fts hit present");
        // FTS hit: original snippet preserved, why gains 'semantic match'.
        assert_eq!(hit["snippet"], "preview of timezone report");
        assert_eq!(
            hit["why"],
            serde_json::json!(["keyword match", "semantic match"])
        );
    }

    #[tokio::test]
    async fn snippet_is_capped_at_200_units() {
        let query_vector = vec![0.5_f32];
        let base_url = spawn_fake_lm_studio(query_vector.clone());
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            configure_lm_studio(&c, &base_url);
            insert_conversation(&c, 3, "Long chunk");
            let long: String = "x".repeat(500);
            insert_indexed_chunk(&c, 3, &long, &[0.5]);
        }
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "anything" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let hits = body["hits"].as_array().unwrap();
        let hit = hits.iter().find(|h| h["id"] == 3).expect("semantic hit");
        let snippet = hit["snippet"].as_str().unwrap();
        assert_eq!(snippet.chars().count(), 200);
        assert!(snippet.chars().all(|c| c == 'x'));
    }

    #[tokio::test]
    async fn qdrant_enabled_uses_the_not_reachable_note() {
        let query_vector = vec![1.0_f32];
        let base_url = spawn_fake_lm_studio(query_vector.clone());
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            configure_lm_studio(&c, &base_url);
            crate::settings::set_bool(&c, "qdrant_enabled", true).unwrap();
            insert_conversation(&c, 4, "Subject");
            insert_indexed_chunk(&c, 4, "content", &[1.0]);
        }
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "anything" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["used_semantic"], true);
        assert_eq!(body["mode_note"], MODE_NOTE_LOCAL_QDRANT_DOWN);
    }

    #[tokio::test]
    async fn scope_customers_skips_the_ticket_hybrid_layer() {
        let query_vector = vec![1.0_f32];
        let base_url = spawn_fake_lm_studio(query_vector.clone());
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            configure_lm_studio(&c, &base_url);
            insert_conversation(&c, 5, "Subject");
            insert_indexed_chunk(&c, 5, "content", &[1.0]);
        }
        let (status, body) = body_json(
            search(
                State(state),
                bytes(&serde_json::json!({ "query": "anything", "scope": "customers" })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["used_semantic"], false);
        // The reference hybrid block only runs for all|tickets scopes; for
        // customers it is skipped entirely — no mode_note is set.
        assert!(body.get("mode_note").is_none() || body["mode_note"].is_null());
    }
}
