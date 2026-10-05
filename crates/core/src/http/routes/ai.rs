//! AI routes — faithful port of `src/server/routes/ai.ts`.
//!
//! v1.6.0 audit semantics preserved: bodies are validated (422 on hostile
//! input, never a 500), provider failures surface as honest 503s with the
//! reference error envelope, and non-integer ids are client errors.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use crate::ai_pipeline::{self, AiBackend, LmStudioError};

/// The reference 422 envelope.
fn validation_422(message: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": message,
        })),
    )
        .into_response()
}

/// The reference 503 envelope for provider-shaped failures.
fn service_503(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "statusCode": 503,
            "error": "ServiceUnavailable",
            "message": message,
        })),
    )
        .into_response()
}

fn not_found_404(message: &str) -> Response {
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

/// GET /api/ai/status — settings + a live LM Studio probe (reference
/// `ctx.lmStudio.listModels()` with the not-reachable error message).
pub async fn status(State(state): State<AppState>) -> Json<Value> {
    let (base_url, chat_model, embedding_model, timeout_ms, concurrency, provider_kind) = {
        let conn = state.conn_lock();
        // Reference: `provider: ctx.aiProvider.kind` — LM Studio whenever
        // ai_enabled (context.ts:208-209), not the AI-Center display row.
        let ai_enabled = crate::settings::get_bool(&conn, "ai_enabled", true).unwrap_or(true);
        let provider_kind = if ai_enabled { "lmstudio" } else { "none" }.to_string();
        let base_url = crate::settings::get_string(&conn, "lmstudio_base_url")
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                crate::ai_center::get_ai_status(&conn)
                    .ok()
                    .and_then(|s| s.base_url)
            })
            .unwrap_or_else(|| "http://127.0.0.1:1234".into());
        let chat_model = crate::settings::get_string(&conn, "lmstudio_chat_model")
            .ok()
            .flatten()
            .map(|s| if s.is_empty() { None } else { Some(s) })
            .unwrap_or_else(|| {
                crate::ai_center::get_ai_status(&conn)
                    .ok()
                    .and_then(|s| s.chat_model)
            });
        let embedding_model = crate::settings::get_string(&conn, "lmstudio_embedding_model")
            .ok()
            .flatten()
            .map(|s| if s.is_empty() { None } else { Some(s) })
            .unwrap_or_else(|| {
                crate::ai_center::get_ai_status(&conn)
                    .ok()
                    .and_then(|s| s.embedding_model)
            });
        let timeout_ms =
            crate::settings::get_i64(&conn, "lmstudio_timeout_ms", 120_000).unwrap_or(120_000);
        let concurrency = crate::settings::get_i64(&conn, "lmstudio_concurrency", 2).unwrap_or(2);
        (
            base_url,
            chat_model,
            embedding_model,
            timeout_ms,
            concurrency,
            provider_kind,
        )
    };
    // Live probe: connected + model list, or the reference error message.
    let client = crate::ai_lm_studio::OpenAiCompatibleClient::new(&base_url);
    let lmstudio = match client.list_models().await {
        Ok(models) => json!({
            "connected": true,
            "models": models.iter().map(|m| json!(m.id)).collect::<Vec<_>>(),
            "error": Value::Null,
        }),
        Err(e) => json!({
            "connected": false,
            "models": [],
            "error": format!(
                "LM Studio is not reachable at {base_url}. Start LM Studio, load a model, and enable the local server (Developer tab > Start Server). Details: {e}"
            ),
        }),
    };
    let conn = state.conn_lock();
    // Reference `lmStudio.getLastInference()` is process-lifetime memory
    // `{ at, latencyMs }`; the port persists runs, so the honest equivalent
    // is the last completed run that recorded a latency (a restart keeps
    // the value, which the reference loses — documented deviation).
    let last_inference: Value = conn
        .query_row(
            "SELECT created_at, latency_ms FROM ai_runs
              WHERE status = 'completed' AND latency_ms IS NOT NULL
              ORDER BY id DESC LIMIT 1",
            [],
            |r| {
                Ok(json!({
                    "at": r.get::<_, String>(0)?,
                    "latencyMs": r.get::<_, i64>(1)?,
                }))
            },
        )
        .unwrap_or(Value::Null);
    // Reference `jobRepo.queueStats()` — ALL jobs (the status row the
    // reference serves is queue health, not an AI-only filter).
    let (queued, running, failed): (i64, i64, i64) = conn
        .query_row(
            "SELECT SUM(CASE WHEN status = 'queued' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END)
             FROM jobs",
            [],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            },
        )
        .unwrap_or((0, 0, 0));
    // Reference `knowledgeRepo.countIndexed()` — thread embeddings over the
    // real `conversation_threads.embedding_state` (M040), knowledge chunks
    // over `knowledge_chunks.embedding_state` (M039).
    let (conversations_indexed, chunks_indexed, chunks_pending, chunks_failed): (
        i64,
        i64,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM conversation_threads
                  WHERE embedding_state = 'indexed'),
                (SELECT COUNT(*) FROM knowledge_chunks
                  WHERE embedding_state = 'indexed'),
                (SELECT COUNT(*) FROM knowledge_chunks
                  WHERE embedding_state IN ('not_indexed', 'queued')),
                (SELECT COUNT(*) FROM knowledge_chunks
                  WHERE embedding_state = 'failed')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap_or((0, 0, 0, 0));
    Json(json!({
        "ai_enabled": provider_kind == "lmstudio",
        "settings": {
            "base_url": base_url,
            "chat_model": chat_model,
            "embedding_model": embedding_model,
            "timeout_ms": timeout_ms,
            "concurrency": concurrency,
        },
        "lmstudio": lmstudio,
        "last_inference": last_inference,
        "provider": provider_kind,
        "queued_ai_jobs": queued + running,
        "failed_ai_jobs": failed,
        "index": {
            "conversations_indexed": conversations_indexed,
            "chunks_indexed": chunks_indexed,
            "chunks_pending": chunks_pending,
            "chunks_failed": chunks_failed,
        },
    }))
}

/// POST /api/ai/analyze/:conversationId — trigger/refresh ticket analysis.
pub async fn analyze(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
    body: Option<Json<Value>>,
) -> Response {
    if conversation_id <= 0 {
        return validation_422("conversationId must be a positive integer.");
    }
    let force = body
        .and_then(|Json(b)| b.get("force").and_then(Value::as_bool))
        .unwrap_or(false);
    let result = state
        .run_ai(move |conn| {
            Box::pin(async move {
                ai_pipeline::ensure_pipeline_schema(conn).ok();
                let backend = ai_pipeline::backend_from_settings(conn);
                ai_pipeline::analyze_ticket(conn, &backend, conversation_id, force).await
            })
        })
        .await;
    match result {
        Ok(Ok(outcome)) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "analysis": outcome.analysis,
                "sources": outcome.sources,
                "cached": outcome.cached,
                "runId": outcome.run_id,
            })),
        )
            .into_response(),
        Ok(Err(e)) => service_503(&e.message),
        Err(join) => service_503(&join),
    }
}

/// POST /api/ai/draft/:conversationId — generate a customer-safe draft
/// (+ verification pass).
pub async fn draft(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
    body: Option<Json<Value>>,
) -> Response {
    if conversation_id <= 0 {
        return validation_422("conversationId must be a positive integer.");
    }
    let body = body.map(|Json(b)| b).unwrap_or_else(|| json!({}));
    // draftGenerateSchema: mode enum + force bool; anything else is a 422.
    let mode = match body.get("mode") {
        None | Some(Value::Null) => "verified_answer",
        Some(Value::String(s)) if s == "verified_answer" || s == "standard" => s,
        Some(_) => return validation_422("mode must be 'verified_answer' or 'standard'."),
    };
    let force = match body.get("force") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return validation_422("force must be a boolean."),
    };
    let mode = mode.to_string();
    let result = state
        .run_ai(move |conn| {
            Box::pin(async move {
                ai_pipeline::ensure_pipeline_schema(conn).ok();
                let backend = ai_pipeline::backend_from_settings(conn);
                ai_pipeline::generate_draft(conn, &backend, conversation_id, &mode, force, None)
                    .await
            })
        })
        .await;
    match result {
        Ok(Ok((draft, verification))) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "draft": draft,
                "verification": verification,
            })),
        )
            .into_response(),
        Ok(Err(e)) => service_503(&e.message),
        Err(join) => service_503(&join),
    }
}

/// POST /api/ai/draft/:draftId/rewrite — rewrite without ever touching the
/// user's composer (spec #37).
pub async fn rewrite(
    State(state): State<AppState>,
    Path(draft_id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    let instruction = match body.get("instruction").and_then(Value::as_str) {
        Some(i) if ["shorten", "expand", "warmer", "more_direct"].contains(&i) => i.to_string(),
        _ => {
            return validation_422(
                "instruction must be one of 'shorten', 'expand', 'warmer', 'more_direct'.",
            )
        }
    };
    let result = state
        .run_ai(move |conn| {
            Box::pin(async move {
                ai_pipeline::ensure_pipeline_schema(conn).ok();
                let backend = ai_pipeline::backend_from_settings(conn);
                ai_pipeline::rewrite_draft(conn, &backend, draft_id, &instruction).await
            })
        })
        .await;
    match result {
        Ok(Ok(text)) => (StatusCode::OK, Json(json!({ "ok": true, "text": text }))).into_response(),
        Ok(Err(e)) => service_503(&e.message),
        Err(join) => service_503(&join),
    }
}

/// POST /api/ai/draft/:draftId/verify — verify a draft again.
pub async fn verify(State(state): State<AppState>, Path(draft_id): Path<i64>) -> Response {
    let prepared = {
        let conn = state.conn_lock();
        ai_pipeline::ensure_pipeline_schema(&conn).ok();
        ai_pipeline::get_draft(&conn, draft_id)
            .ok()
            .flatten()
            .map(|d| (d.conversation_id, d.content))
    };
    let Some((conversation_id, content)) = prepared else {
        return not_found_404("Draft not found.");
    };
    let result = state
        .run_ai(move |conn| {
            Box::pin(async move {
                let backend = ai_pipeline::backend_from_settings(conn);
                let analysis = ai_pipeline::get_latest_analysis(conn, conversation_id)
                    .ok()
                    .flatten()
                    .map(|a| a.analysis);
                ai_pipeline::verify_draft_internal(
                    conn,
                    &backend,
                    conversation_id,
                    &content,
                    analysis.as_ref(),
                )
                .await
            })
        })
        .await;
    match result {
        Ok(Ok(verification)) => {
            let conn = state.conn_lock();
            let _ = conn.execute(
                "UPDATE ai_drafts SET verification = ?1 WHERE id = ?2",
                rusqlite::params![
                    serde_json::to_string(&verification).unwrap_or_default(),
                    draft_id
                ],
            );
            (
                StatusCode::OK,
                Json(json!({ "ok": true, "verification": verification })),
            )
                .into_response()
        }
        Ok(Err(e)) => service_503(&e.message),
        Err(join) => service_503(&join),
    }
}

/// POST /api/ai/draft/:draftId/feedback — accept/reject/edit (spec #123).
pub async fn feedback(
    State(state): State<AppState>,
    Path(draft_id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    let action = match body.get("action").and_then(Value::as_str) {
        Some(a) if ["accept", "reject", "edit"].contains(&a) => a.to_string(),
        _ => return validation_422("action must be 'accept', 'reject' or 'edit'."),
    };
    let final_text = body
        .get("finalText")
        .and_then(Value::as_str)
        .map(str::to_string);
    if action == "edit" && body.get("finalText").is_some() && final_text.is_none() {
        return validation_422("finalText must be a string.");
    }
    let conn = state.conn_lock();
    ai_pipeline::ensure_pipeline_schema(&conn).ok();
    let Some(draft) = ai_pipeline::get_draft(&conn, draft_id).ok().flatten() else {
        return Json(json!({ "ok": false, "message": "Draft not found." })).into_response();
    };
    match action.as_str() {
        "accept" => {
            let _ = ai_pipeline::set_draft_state(&conn, draft_id, "accepted");
        }
        "reject" => {
            let _ = ai_pipeline::set_draft_state(&conn, draft_id, "rejected");
        }
        "edit" => {
            let _ = ai_pipeline::set_draft_state(&conn, draft_id, "edited");
            if let Some(final_text) = final_text.as_deref() {
                let _ = ai_pipeline::record_feedback(
                    &conn,
                    draft_id,
                    &draft.content,
                    final_text,
                    false,
                );
            }
        }
        _ => unreachable!(),
    }
    let _ = crate::audit::audit(
        &conn,
        &crate::audit::AuditEntry {
            actor: "user",
            action: format!("ai_draft_{action}"),
            conversation_id: Some(draft.conversation_id),
            before_state: None,
            after_state: None,
            remote_operation: None,
            remote_result: None,
            ai_involvement: true,
            job_id: None,
            correlation_id: None,
        },
    );
    let message = format!(
        "Draft marked as {}.",
        if action == "edit" {
            "edited".to_string()
        } else {
            format!("{action}ed")
        }
    );
    Json(json!({ "ok": true, "message": message })).into_response()
}

/// GET /api/ai/similar/:conversationId — similar conversations (hybrid
/// relevance).
pub async fn similar(State(state): State<AppState>, Path(conversation_id): Path<i64>) -> Response {
    if conversation_id <= 0 {
        return validation_422("conversationId must be a positive integer.");
    }
    let conn = state.conn_lock();
    ai_pipeline::ensure_pipeline_schema(&conn).ok();
    let similar: Vec<Value> = crate::ai_evidence::find_similar(&conn, conversation_id, 5, &[])
        .unwrap_or_default()
        .iter()
        .map(|s| {
            json!({
                "conversation_id": s.conversation_id,
                "number": s.number,
                "subject": s.subject,
                "resolution": s.resolution,
                "date": s.date,
                "status": s.status,
                "score": s.score,
                "why": s.why,
            })
        })
        .collect();
    Json(json!({ "similar": similar })).into_response()
}

/// GET /api/ai/memory/:customerId — memories, with quarantined red-line
/// entries never returned as usable memory.
pub async fn get_memory(State(state): State<AppState>, Path(customer_id): Path<i64>) -> Response {
    if customer_id <= 0 {
        return validation_422("customerId must be a positive integer.");
    }
    let conn = state.conn_lock();
    ai_pipeline::ensure_pipeline_schema(&conn).ok();
    let memories: Vec<Value> = conn
        .prepare(
            "SELECT id, customer_id, memory_key, memory_value, source, confidence, origin, created_at
               FROM customer_memory WHERE customer_id = ?1
              ORDER BY COALESCE(last_seen_at, created_at) DESC",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![customer_id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "customer_id": r.get::<_, i64>(1)?,
                    "key": r.get::<_, String>(2)?,
                    "value": r.get::<_, Option<String>>(3)?,
                    "source": r.get::<_, String>(4)?,
                    "confidence": r.get::<_, Option<String>>(5)?,
                    "origin": r.get::<_, Option<String>>(6)?,
                    "created_at": r.get::<_, String>(7)?,
                }))
            })
            .ok()
            .map(|rows| {
                rows.filter_map(|r| r.ok())
                    .filter(|m| {
                        !super::memory::is_quarantined(
                            m["key"].as_str().unwrap_or(""),
                            m["value"].as_str(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({ "memories": memories, "customerId": customer_id })).into_response()
}

/// POST /api/ai/memory/:customerId — human memory entry. Delegates to the
/// same service semantics as /api/memory (v2.2.1 audit fix): personality
/// red-line refusal (422), customer-existence check (404, was a raw FK 500)
/// and honest source labeling.
pub async fn set_memory(
    State(state): State<AppState>,
    Path(customer_id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    if customer_id <= 0 {
        return validation_422("customerId must be a positive integer.");
    }
    let key = body.get("key").and_then(Value::as_str).unwrap_or_default();
    let value = body
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if key.is_empty() || key.chars().count() > 200 {
        return validation_422("key must be a non-empty string of at most 200 characters.");
    }
    if value.chars().count() > 5000 {
        return validation_422("value must be at most 5000 characters.");
    }
    let conn = state.conn_lock();
    ai_pipeline::ensure_pipeline_schema(&conn).ok();
    let customer_exists = conn
        .query_row(
            "SELECT 1 FROM customers WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![customer_id],
            |_| Ok(()),
        )
        .is_ok();
    if !customer_exists {
        return not_found_404("Customer not found.");
    }
    if super::memory::is_quarantined(key, Some(value)) {
        return validation_422(
            "SupportOS policy: psychological/personality judgments are never stored as customer memory. Rephrase as an observable fact.",
        );
    }
    let _ = ai_pipeline::upsert_memory(
        &conn,
        customer_id,
        key,
        value,
        &ai_pipeline::UpsertMemoryOpts {
            source: "human",
            origin: "manual",
            conversation_id: None,
            confidence: "high",
        },
    );
    let _ = crate::audit::audit(
        &conn,
        &crate::audit::AuditEntry {
            actor: "user",
            action: "memory_added".into(),
            conversation_id: None,
            before_state: None,
            after_state: None,
            remote_operation: None,
            remote_result: None,
            ai_involvement: false,
            job_id: None,
            correlation_id: None,
        },
    );
    Json(json!({ "ok": true, "message": "Memory saved (human-entered)." })).into_response()
}

/// POST /api/ai/cluster-issues — run the clustering stage.
pub async fn cluster_issues(State(state): State<AppState>, body: Option<Json<Value>>) -> Response {
    let days = body
        .map(|Json(b)| b.get("days").and_then(Value::as_i64))
        .unwrap_or(None);
    let days = match days {
        None => 60,
        Some(d) if (1..=3650).contains(&d) => d,
        Some(_) => return validation_422("days must be an integer between 1 and 3650."),
    };
    let result = state
        .run_ai(move |conn| {
            Box::pin(async move {
                ai_pipeline::ensure_pipeline_schema(conn).ok();
                let backend = ai_pipeline::backend_from_settings(conn);
                ai_pipeline::cluster_issues(conn, &backend, days).await
            })
        })
        .await;
    match result {
        Ok(Ok(clusters)) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "clusters": clusters,
            })),
        )
            .into_response(),
        Ok(Err(e)) => service_503(&e.message),
        Err(join) => service_503(&join),
    }
}

/// GET /api/ai/jobs — reference `aiRepo.listJobs`: the AI run history
/// from `ai_runs` (the pipeline's own ledger), NOT the job queue.
pub async fn jobs(
    State(state): State<AppState>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Json<Value> {
    let limit = query
        .as_deref()
        .and_then(|q| q.split('&').find_map(|p| p.strip_prefix("limit=")))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let conn = state.conn_lock();
    let jobs: Vec<Value> = conn
        .prepare(
            "SELECT id, type, status, conversation_id, model, prompt_version, error,
                    created_at, started_at, completed_at, latency_ms
               FROM ai_runs ORDER BY id DESC LIMIT ?1",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "type": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "conversation_id": r.get::<_, Option<i64>>(3)?,
                    "model": r.get::<_, Option<String>>(4)?,
                    "prompt_version": r.get::<_, Option<String>>(5)?,
                    "error": r.get::<_, Option<String>>(6)?,
                    "created_at": r.get::<_, String>(7)?,
                    "started_at": r.get::<_, Option<String>>(8)?,
                    "completed_at": r.get::<_, Option<String>>(9)?,
                    "latency_ms": r.get::<_, Option<i64>>(10)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({ "jobs": jobs }))
}

/// GET /api/ai/analytics — same shape as /api/analytics/ai. Reference
/// `aiRepo.aiAnalytics()` + `analyticsService.aiAnalytics()`: every metric
/// is a stored-data read (ai_runs / ai_drafts / ai_feedback /
/// ai_verifications), rates are rounded percents, and the failure patterns
/// come from the first verification warning of each draft.
pub async fn ai_analytics(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn_lock();
    let (tickets_analyzed, analysis_completed, analysis_failed): (i64, i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT COUNT(DISTINCT conversation_id) FROM ai_runs
                  WHERE type = 'ticket_analysis' AND status = 'completed'
                    AND conversation_id IS NOT NULL),
                (SELECT COUNT(*) FROM ai_runs
                  WHERE type = 'ticket_analysis' AND status = 'completed'),
                (SELECT COUNT(*) FROM ai_runs
                  WHERE type = 'ticket_analysis' AND status = 'failed')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((0, 0, 0));
    let (draft_count, drafts_accepted, drafts_rejected, drafts_edited): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM ai_drafts),
                (SELECT COUNT(*) FROM ai_drafts WHERE state = 'accepted'),
                (SELECT COUNT(*) FROM ai_drafts WHERE state = 'rejected'),
                (SELECT COUNT(DISTINCT f.draft_id) FROM ai_feedback f
                   JOIN ai_drafts d ON d.id = f.draft_id
                  WHERE f.edit_distance > 20)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap_or((0, 0, 0, 0));
    let (verification_warnings, unsupported_claims): (i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM ai_verifications
                  WHERE verified = 1 AND warnings != '[]'),
                (SELECT COUNT(*) FROM ai_verifications
                  WHERE unsupported_claims != '[]')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    let analysis_success_rate = if analysis_completed + analysis_failed > 0 {
        ((analysis_completed as f64) / ((analysis_completed + analysis_failed) as f64) * 1000.0)
            .round()
            / 10.0
    } else {
        0.0
    };
    let feedback_total = drafts_accepted + drafts_rejected + drafts_edited;
    let draft_edit_rate = if feedback_total > 0 {
        ((drafts_edited as f64) / (feedback_total as f64) * 1000.0).round() / 10.0
    } else {
        0.0
    };
    let unsupported_claim_rate = if draft_count > 0 {
        ((unsupported_claims as f64) / (draft_count as f64) * 1000.0).round() / 10.0
    } else {
        0.0
    };
    let patterns: Vec<Value> = conn
        .prepare(
            "SELECT json_extract(verification, '$.warnings[0]') AS pattern, COUNT(*) AS count
               FROM ai_drafts
              WHERE verification IS NOT NULL
                AND json_extract(verification, '$.warnings[0]') IS NOT NULL
              GROUP BY pattern ORDER BY count DESC LIMIT 5",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "pattern": r.get::<_, String>(0)?,
                    "count": r.get::<_, i64>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "tickets_analyzed": tickets_analyzed,
        "analysis_success_rate": analysis_success_rate,
        "draft_count": draft_count,
        "draft_accepted": drafts_accepted,
        "draft_rejected": drafts_rejected,
        "draft_edit_rate": draft_edit_rate,
        "verification_warnings": verification_warnings,
        "unsupported_claim_rate": unsupported_claim_rate,
        "common_failure_patterns": patterns,
        "source": "local",
    }))
}

/// GET /api/ai/evaluation — the golden test set (spec #78, #79) + the
/// configured evaluation mode (reference `getAllSettings().ai_evaluation_mode`
/// — a boolean, default false; the checkbox in the UI depends on it).
pub async fn evaluation(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn_lock();
    let mode = crate::settings::get_bool(&conn, "ai_evaluation_mode", false).unwrap_or(false);
    let tests = [
        (
            "simple question",
            "simple",
            "What time do you close?",
            "Hi, what are your support hours?",
        ),
        (
            "multi-question ticket",
            "multi",
            "Two things: export + timezone",
            "How do I export data? Also how do I change the timezone for scheduled reports?",
        ),
        (
            "ambiguous ticket",
            "ambiguous",
            "It does not work",
            "The thing keeps failing sometimes. Not sure what is wrong.",
        ),
        (
            "known issue",
            "known_issue",
            "Meeting reminders one hour late",
            "Since the DST change our reminders are all one hour late.",
        ),
        (
            "customer history",
            "history",
            "Follow-up on the export issue",
            "The export you helped me with last month broke again.",
        ),
        (
            "timezone issue",
            "timezone",
            "Santiago timezone wrong",
            "Scheduled report sends at 3 AM instead of 8 AM Chile time.",
        ),
        (
            "integration issue",
            "integration",
            "Slack integration broken",
            "The Slack integration stopped posting updates to our channel.",
        ),
        (
            "billing question",
            "billing",
            "Card declined",
            "My payment failed but the card works everywhere else.",
        ),
        (
            "internal escalation",
            "escalation",
            "URGENT outage for key account",
            "Our production access is down, we need this escalated now.",
        ),
    ];
    Json(json!({
        "tests": tests.iter().map(|(name, category, subject, body)| json!({
            "name": name,
            "category": category,
            "payload": { "subject": subject, "body": body },
        })).collect::<Vec<_>>(),
        "evaluation_mode": mode,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::{IntoResponse, Response};
    use std::sync::{Arc, Mutex};

    fn make_state() -> AppState {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        AppState {
            conn: Arc::new(Mutex::new(conn)),
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
            qdrant: Arc::new(crate::vectorstore_qdrant::EmbeddedQdrant::new(
                "/tmp/spp-test-qdrant",
                "http://127.0.0.1:6333",
                false,
            )),
        }
    }

    async fn body_json(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    fn seed_conversation(state: &AppState, remote_id: i64) -> i64 {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id, created_at)
             VALUES (?1, ?1, 'active', 1, 3001, '2026-10-01T00:00:00Z')",
            rusqlite::params![remote_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// The status row must serve the reference shape with REAL counts: the
    /// queue stats over every job, `last_inference` as `{at, latencyMs}`
    /// from the last completed run, and the index block over
    /// `conversation_threads.embedding_state` / `knowledge_chunks.embedding_state`.
    #[tokio::test]
    async fn status_serves_reference_shape_with_real_counts() {
        let state = make_state();
        let conv = {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id, created_at)
                 VALUES (9001, 9001, 'active', 1, 3001, '2026-10-01T00:00:00Z')",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            // Queue: 1 queued + 1 running (→ queued_ai_jobs 2), 1 failed.
            for (queue, kind, status) in [
                ("default", "sync_conversation", "queued"),
                ("default", "sync_conversation", "running"),
                ("default", "sync_conversation", "failed"),
                ("default", "sync_conversation", "completed"),
            ] {
                conn.execute(
                    "INSERT INTO jobs (queue, type, status) VALUES (?1, ?2, ?3)",
                    rusqlite::params![queue, kind, status],
                )
                .unwrap();
            }
            // Last inference: the newest completed run with a latency wins.
            conn.execute(
                "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type,
                                      conversation_id, status, latency_ms, created_at)
                 VALUES ('h1', 'v1', 'test-model', '{}', 'ticket_analysis', NULL,
                         'completed', 812, '2026-10-04T10:00:00Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type,
                                      conversation_id, status, latency_ms, created_at)
                 VALUES ('h2', 'v1', 'test-model', '{}', 'ticket_analysis', NULL,
                         'completed', 410, '2026-10-05T09:30:00Z')",
                [],
            )
            .unwrap();
            // Index: one indexed thread + one pending; one indexed chunk +
            // one failed chunk (via a real document under a real source).
            for (body, emb) in [("hello", "indexed"), ("world", "not_indexed")] {
                conn.execute(
                    "INSERT INTO conversation_threads (conversation_id, thread_type, body, actor_type, embedding_state)
                     VALUES (?1, 'customer', ?2, 'customer', ?3)",
                    rusqlite::params![conv, body, emb],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO knowledge_sources (name, kind) VALUES ('src', 'local_file')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO knowledge_documents (source_id, title) VALUES (1, 'doc')",
                [],
            )
            .unwrap();
            for emb in ["indexed", "failed"] {
                conn.execute(
                    "INSERT INTO knowledge_chunks (document_id, chunk_index, content, embedding_state)
                     VALUES (1, ?1, 'chunk', ?2)",
                    rusqlite::params![if emb == "indexed" { 0 } else { 1 }, emb],
                )
                .unwrap();
            }
        }

        let (status, body) = body_json(status(State(state)).await.into_response()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ai_enabled"], json!(true));
        assert_eq!(body["provider"], json!("lmstudio"));
        // Reference getLmStudio shape.
        assert_eq!(body["settings"]["base_url"], json!("http://127.0.0.1:1234"));
        assert_eq!(body["settings"]["timeout_ms"], json!(120_000));
        assert_eq!(body["settings"]["concurrency"], json!(2));
        assert!(body["settings"]["chat_model"].is_null());
        // No LM Studio in the test environment: honest offline probe.
        assert_eq!(body["lmstudio"]["connected"], json!(false));
        assert!(body["lmstudio"]["error"].is_string());
        // Queue stats over ALL jobs.
        assert_eq!(body["queued_ai_jobs"], json!(2));
        assert_eq!(body["failed_ai_jobs"], json!(1));
        // last_inference is the reference {at, latencyMs} object from the
        // newest completed run, not a bare string.
        assert_eq!(body["last_inference"]["at"], json!("2026-10-05T09:30:00Z"));
        assert_eq!(body["last_inference"]["latencyMs"], json!(410));
        // countIndexed over the real embedding-state columns.
        assert_eq!(body["index"]["conversations_indexed"], json!(1));
        assert_eq!(body["index"]["chunks_indexed"], json!(1));
        assert_eq!(body["index"]["chunks_failed"], json!(1));
    }

    /// `/api/ai/jobs` is the reference `aiRepo.listJobs`: the `ai_runs`
    /// ledger (NOT the job queue), newest first, with the reference columns.
    #[tokio::test]
    async fn jobs_lists_the_ai_runs_ledger() {
        let state = make_state();
        let conv = seed_conversation(&state, 9002);
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type,
                                      conversation_id, status, latency_ms, created_at)
                 VALUES ('h1', 'v1', 'm', '{}', 'ticket_analysis', ?1, 'completed', 900, '2026-10-04T10:00:00Z')",
                rusqlite::params![conv],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type,
                                      status, error, created_at)
                 VALUES ('h2', 'v1', 'm', '{}', 'draft', 'failed', 'boom', '2026-10-05T11:00:00Z')",
                [],
            )
            .unwrap();
        }

        let (status, body) = body_json(
            jobs(State(state), axum::extract::RawQuery(None))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let jobs = body["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 2);
        // Newest first.
        assert_eq!(jobs[0]["id"], json!(2));
        assert_eq!(jobs[0]["type"], json!("draft"));
        assert_eq!(jobs[0]["status"], json!("failed"));
        assert_eq!(jobs[0]["error"], json!("boom"));
        assert!(jobs[0]["conversation_id"].is_null());
        assert_eq!(jobs[1]["type"], json!("ticket_analysis"));
        assert_eq!(jobs[1]["status"], json!("completed"));
        assert_eq!(jobs[1]["conversation_id"], json!(conv));
        assert_eq!(jobs[1]["model"], json!("m"));
        assert_eq!(jobs[1]["latency_ms"], json!(900));
    }

    /// `/api/ai/analytics` computes every reference metric from stored data:
    /// distinct-conversation analyses, draft states, edit-distance feedback,
    /// verification warnings and the failure-pattern rollup.
    #[tokio::test]
    async fn analytics_computes_the_reference_metrics() {
        let state = make_state();
        let (conv_a, conv_b) = (
            seed_conversation(&state, 9101),
            seed_conversation(&state, 9102),
        );
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            // 3 completed analyses over 2 distinct conversations + 1 failed.
            for (i, conv) in [Some(conv_a), Some(conv_b), Some(conv_a), None]
                .iter()
                .enumerate()
            {
                conn.execute(
                    "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type,
                                          conversation_id, status, created_at)
                     VALUES (?1, 'v1', 'm', '{}', 'ticket_analysis', ?2, ?3, '2026-10-04T10:00:00Z')",
                    rusqlite::params![format!("h{i}"), conv,
                        if i == 3 { "failed" } else { "completed" }],
                )
                .unwrap();
            }
            // 4 drafts: accepted, rejected, and two carrying verification
            // warnings (the failure-pattern source). ai_drafts.verification
            // stores the object; ai_verifications.warnings stores the array.
            let drafts = [
                ("accepted", serde_json::json!({ "warnings": [] })),
                (
                    "rejected",
                    serde_json::json!({ "warnings": ["invented timeframe"] }),
                ),
                (
                    "generated",
                    serde_json::json!({ "warnings": ["invented timeframe"] }),
                ),
                (
                    "generated",
                    serde_json::json!({ "warnings": ["unanswered question"] }),
                ),
            ];
            for (i, (state_, verification)) in drafts.iter().enumerate() {
                let warnings_array = verification["warnings"].to_string();
                conn.execute(
                    "INSERT INTO ai_drafts (conversation_id, content, state, verification)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        conv_a,
                        format!("draft {i}"),
                        state_,
                        verification.to_string()
                    ],
                )
                .unwrap();
                let draft_id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO ai_verifications (draft_id, verified, unsupported_claims, warnings)
                     VALUES (?1, 1, ?2, ?3)",
                    rusqlite::params![
                        draft_id,
                        if i == 1 { "[\"claim\"]" } else { "[]" },
                        warnings_array
                    ],
                )
                .unwrap();
            }
            // One heavily-edited draft (edit_distance > 20 → drafts_edited).
            conn.execute(
                "INSERT INTO ai_feedback (draft_id, original_content, final_content, edit_distance, was_sent)
                 VALUES (1, 'a', 'b', 45, 1)",
                [],
            )
            .unwrap();
        }

        let (status, body) = body_json(ai_analytics(State(state)).await.into_response()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["tickets_analyzed"], json!(2));
        // 3 completed / (3 + 1 failed) = 75.0%.
        assert_eq!(body["analysis_success_rate"], json!(75.0));
        assert_eq!(body["draft_count"], json!(4));
        assert_eq!(body["draft_accepted"], json!(1));
        assert_eq!(body["draft_rejected"], json!(1));
        // 1 edited / (1 accepted + 1 rejected + 1 edited) = 33.3%.
        assert_eq!(body["draft_edit_rate"], json!(33.3));
        // 3 verified rows with non-empty warnings (drafts 1-3; draft 0
        // stores the empty array).
        assert_eq!(body["verification_warnings"], json!(3));
        // 1 unsupported claim / 4 drafts = 25.0%.
        assert_eq!(body["unsupported_claim_rate"], json!(25.0));
        assert_eq!(body["source"], json!("local"));
        let patterns = body["common_failure_patterns"].as_array().unwrap();
        assert_eq!(patterns.len(), 2);
        // "invented timeframe" appears twice → first, with its count.
        assert_eq!(patterns[0]["pattern"], json!("invented timeframe"));
        assert_eq!(patterns[0]["count"], json!(2));
        assert_eq!(patterns[1]["pattern"], json!("unanswered question"));
        assert_eq!(patterns[1]["count"], json!(1));
    }

    /// `evaluation_mode` is a boolean (the reference serves the settings
    /// flag; the UI checkbox depends on it), defaulting to false.
    #[tokio::test]
    async fn evaluation_mode_is_a_boolean_defaulting_off() {
        let state = make_state();
        let (_, body) = body_json(evaluation(State(state)).await.into_response()).await;
        assert_eq!(body["evaluation_mode"], json!(false));
        assert_eq!(body["tests"].as_array().unwrap().len(), 9);

        let state = make_state();
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::settings::set_bool(&conn, "ai_evaluation_mode", true).unwrap();
        }
        let (_, body) = body_json(evaluation(State(state)).await.into_response()).await;
        assert_eq!(body["evaluation_mode"], json!(true));
    }
}
