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
    let last_inference: Option<String> = conn
        .query_row("SELECT MAX(created_at) FROM ai_runs", [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten();
    let (queued_ai_jobs, failed_ai_jobs): (i64, i64) = conn
        .query_row(
            "SELECT SUM(CASE WHEN state IN ('queued','running') THEN 1 ELSE 0 END),
                    SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END)
             FROM jobs WHERE kind LIKE 'ai%'",
            [],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                ))
            },
        )
        .unwrap_or((0, 0));
    let conversations_indexed: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations_fts", [], |r| r.get(0))
        .unwrap_or(0);
    let chunks_indexed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_runs WHERE type = 'embedding'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
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
        "queued_ai_jobs": queued_ai_jobs,
        "failed_ai_jobs": failed_ai_jobs,
        "index": {
            "conversations_indexed": conversations_indexed,
            "chunks_indexed": chunks_indexed,
            "chunks_pending": 0,
            "chunks_failed": 0,
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

/// GET /api/ai/jobs — the AI job list (`?limit=` clamped 1..=1000).
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
            "SELECT id, kind, state, attempts, available_at, claimed_at, completed_at, last_error
               FROM jobs WHERE kind LIKE 'ai%' ORDER BY id DESC LIMIT ?1",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "attempts": r.get::<_, i64>(3)?,
                    "created_at": r.get::<_, Option<String>>(4)?,
                    "claimed_at": r.get::<_, Option<String>>(5)?,
                    "completed_at": r.get::<_, Option<String>>(6)?,
                    "error": r.get::<_, Option<String>>(7)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({ "jobs": jobs }))
}

/// GET /api/ai/analytics — same shape as /api/analytics/ai.
pub async fn ai_analytics(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn_lock();
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM ai_runs", [], |r| r.get(0))
        .unwrap_or(0);
    let successful: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_runs WHERE response_json IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let draft_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM ai_drafts", [], |r| r.get(0))
        .unwrap_or(0);
    let draft_accepted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_drafts WHERE state = 'accepted'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let draft_rejected: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_drafts WHERE state = 'rejected'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let analysis_success_rate = if total > 0 {
        (successful as f64) / (total as f64)
    } else {
        0.0
    };
    Json(json!({
        "tickets_analyzed": total,
        "analysis_success_rate": analysis_success_rate,
        "draft_count": draft_count,
        "draft_accepted": draft_accepted,
        "draft_rejected": draft_rejected,
        "draft_edit_rate": 0.0,
        "verification_warnings": 0,
        "unsupported_claim_rate": 0,
        "common_failure_patterns": [],
        "source": "local",
    }))
}

/// GET /api/ai/evaluation — the golden test set (spec #78, #79) + the
/// configured evaluation mode.
pub async fn evaluation(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn_lock();
    let mode = crate::settings::get_string(&conn, "ai_evaluation_mode")
        .ok()
        .flatten()
        .unwrap_or_else(|| "off".into());
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
