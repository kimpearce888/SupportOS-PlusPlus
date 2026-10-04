//! Translation routes — mirrors src/server/routes/translation.ts
//!
//! M5 translation routes (v2.1.0, plan Phase 30). Detection is deterministic
//! and always available; translation requires the local model and reports an
//! honest 503 when AI is disabled or LM Studio is down — no cloud fallback
//! ever exists. Nothing here sends anything: translated drafts return for
//! side-by-side review, and sending still goes through the human write path.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use crate::translation;

/// Zod parse failure — the reference surfaces these as 422 ValidationError
/// (verified by the v2.1 e2e hardening suite).
fn validation_422(message: impl Into<String>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": message.into(),
        })),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": "Conversation not found."
        })),
    )
        .into_response()
}

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

fn bad_gateway(message: String) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "statusCode": 502,
            "error": "BadGateway",
            "message": message,
        })),
    )
        .into_response()
}

/// `Number(request.params.id)` + the reference's explicit positive-integer
/// check (routes/translation.ts:38).
fn path_positive_int(raw: &str) -> Result<i64, Response> {
    match crate::conversation_ops::js_number(raw) {
        Some(v) if v.fract() == 0.0 && v > 0.0 && v <= i64::MAX as f64 => Ok(v as i64),
        _ => Err(validation_422(
            "Conversation id must be a positive integer.",
        )),
    }
}

/// GET /api/translation/meta — languages + the agent's drafting language.
pub async fn meta(State(state): State<AppState>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let languages: Vec<Value> = translation::SUPPORTED_LANGUAGES
        .iter()
        .map(|(code, name)| json!({ "code": code, "name": name }))
        .collect();
    Json(json!({
        "languages": languages,
        "agent_language": translation::agent_language(&conn),
        "note": "Detection is deterministic (script ranges + function words). Translation runs on the locally configured model only; nothing is ever sent automatically."
    }))
    .into_response()
}

/// POST /api/translation/detect — deterministic detection of up to 50 texts.
pub async fn detect(State(_state): State<AppState>, body: Option<Json<Value>>) -> Response {
    let Some(Json(body)) = body else {
        return validation_422("body is required.");
    };
    // zod: texts = z.array(z.string().max(8000)).min(1).max(50)
    let Some(texts) = body.get("texts").and_then(|v| v.as_array()) else {
        return validation_422("texts must be an array of strings.");
    };
    if texts.is_empty() || texts.len() > 50 {
        return validation_422("texts must contain between 1 and 50 strings.");
    }
    let mut parsed: Vec<String> = Vec::with_capacity(texts.len());
    for t in texts {
        let Some(s) = t.as_str() else {
            return validation_422("texts must be an array of strings.");
        };
        if s.chars().count() > 8000 {
            return validation_422("Each text must contain at most 8000 characters.");
        }
        parsed.push(s.to_string());
    }
    Json(json!({ "detections": translation::detect(&parsed) })).into_response()
}

/// GET /api/translation/conversation/:id — per-message detections + primary.
pub async fn conversation(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let id = match path_positive_int(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match translation::conversation_languages(&conn, id) {
        Ok(Some(summary)) => Json(summary).into_response(),
        Ok(None) => not_found(),
        Err(e) => bad_gateway(e.to_string()),
    }
}

/// POST /api/translation/translate — local model only, cached, honest 503s.
pub async fn translate(State(state): State<AppState>, body: Option<Json<Value>>) -> Response {
    let Some(Json(body)) = body else {
        return validation_422("body is required.");
    };
    // zod: { text: string 1..=8000, from: string ≤8 nullable optional,
    //        to: string 2..=8, purpose: enum optional }
    let text = match body.get("text") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 8000 => s.clone(),
        _ => return validation_422("text must be a string of 1 to 8000 characters."),
    };
    let from = match body.get("from") {
        None => None,
        Some(Value::Null) => None,
        Some(Value::String(s)) if s.chars().count() <= 8 => Some(s.clone()),
        Some(_) => {
            return validation_422("from must be a language code (at most 8 characters) or null.")
        }
    };
    let to = match body.get("to") {
        Some(Value::String(s)) if (2..=8).contains(&s.chars().count()) => s.clone(),
        _ => return validation_422("to must be a language code (2 to 8 characters)."),
    };
    let purpose = match body.get("purpose") {
        None => None,
        Some(Value::Null) => None,
        Some(Value::String(s))
            if s == "customer_inbound" || s == "agent_draft" || s == "general" =>
        {
            Some(s.clone())
        }
        Some(_) => {
            return validation_422(
                "purpose must be one of 'customer_inbound', 'agent_draft', 'general'.",
            )
        }
    };
    if !translation::SUPPORTED_LANGUAGES
        .iter()
        .any(|(code, _)| *code == to)
    {
        return validation_422(format!(
            "Unsupported target language. Supported: {}.",
            translation::SUPPORTED_LANGUAGES
                .iter()
                .map(|(c, _)| *c)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // Honest 503 when AI is disabled (no cloud fallback ever exists).
    {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let ai_enabled = crate::settings::get_bool(&conn, "ai_enabled", true).unwrap_or(true);
        if !ai_enabled {
            return service_503(
                "AI is disabled in Settings. Translation uses the local LM Studio model only - there is no cloud fallback, so translation is unavailable.",
            );
        }
    }
    // The chat call holds the SQLite connection across the LM Studio await —
    // same bridge the AI pipeline routes use (run_ai).
    let outcome = state
        .run_ai(move |conn| {
            Box::pin(async move {
                crate::translation::ensure_translation_schema(conn).ok();
                let backend = crate::ai_pipeline::backend_from_settings(conn);
                let chat = |messages: Vec<crate::ai_provider::ChatMessage>,
                            temperature: f64,
                            max_tokens: u32| {
                    let backend = &backend;
                    Box::pin(async move {
                        backend
                            .chat_qa(messages, temperature, max_tokens, false)
                            .await
                            .map(|res| (res.content, res.model))
                            .map_err(|e| e.message)
                    })
                        as std::pin::Pin<
                            Box<
                                dyn std::future::Future<
                                        Output = std::result::Result<
                                            (Option<String>, String),
                                            String,
                                        >,
                                    > + '_,
                            >,
                        >
                };
                translation::translate(conn, &chat, &text, from.as_deref(), &to, purpose.as_deref())
                    .await
            })
        })
        .await;
    match outcome {
        Ok(Ok(result)) => {
            let mut out = json!({ "ok": true });
            if let (Value::Object(base), Value::Object(extra)) = (out.clone(), result) {
                let mut merged = base;
                for (k, v) in extra {
                    merged.insert(k, v);
                }
                out = Value::Object(merged);
            }
            Json(out).into_response()
        }
        Ok(Err(crate::translation::TranslateError::Client(message))) => validation_422(message),
        Ok(Err(crate::translation::TranslateError::Service(message))) => service_503(&message),
        Ok(Err(crate::translation::TranslateError::BadGateway(message))) => bad_gateway(message),
        Err(join) => bad_gateway(join),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn fresh_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        crate::translation::ensure_translation_schema(&conn).unwrap();
        conn
    }

    fn make_state() -> AppState {
        AppState {
            conn: Arc::new(Mutex::new(fresh_db())),
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

    #[tokio::test]
    async fn meta_lists_languages_and_agent_language() {
        let state = make_state();
        let (status, body) = body_json(meta(State(state)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["languages"].as_array().unwrap().len() >= 18);
        assert_eq!(body["agent_language"]["code"], json!("en"));
        assert!(body["note"]
            .as_str()
            .unwrap()
            .contains("nothing is ever sent automatically"));
    }

    #[tokio::test]
    async fn detect_classifies_and_hardens_hostile_payloads() {
        let state = make_state();
        let (status, body) = body_json(
            detect(
                State(state.clone()),
                Some(Json(json!({
                    "texts": [
                        "Hola, no puedo entrar en mi cuenta",
                        "Hello there, my account is broken"
                    ]
                }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let detections = body["detections"].as_array().unwrap();
        assert_eq!(detections[0]["code"], json!("es"));
        assert_eq!(detections[1]["code"], json!("en"));

        // v2.1 e2e hardening: hostile payloads are 422.
        let (status, _) =
            body_json(detect(State(state.clone()), Some(Json(json!({ "texts": [] })))).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = body_json(
            detect(
                State(state.clone()),
                Some(Json(json!({ "texts": "not-an-array" }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn translate_hardening_matches_the_reference_statuses() {
        let state = make_state();
        // Unsupported target language → 422.
        let (status, body) = body_json(
            translate(
                State(state.clone()),
                Some(Json(json!({ "text": "Hello", "to": "xx" }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], json!("ValidationError"));

        // Empty text → 422 (zod min(1)).
        let (status, _) = body_json(
            translate(
                State(state.clone()),
                Some(Json(json!({ "text": "", "to": "fr" }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        // Over-length text → 422 (zod max(8000)).
        let (status, _) = body_json(
            translate(
                State(state.clone()),
                Some(Json(json!({ "text": "a".repeat(9000), "to": "fr" }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        // AI disabled → honest 503, no cloud fallback.
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::settings::set_bool(&conn, "ai_enabled", false).unwrap();
        }
        let (status, body) = body_json(
            translate(
                State(state.clone()),
                Some(Json(
                    json!({ "text": "Hello, my account is broken", "to": "fr" }),
                )),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("no cloud fallback"));
    }

    #[tokio::test]
    async fn conversation_requires_positive_integer_and_existing_conversation() {
        let state = make_state();
        // Non-integer / non-positive → 422.
        let (status, _) =
            body_json(conversation(State(state.clone()), Path("abc".into())).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) =
            body_json(conversation(State(state.clone()), Path("0".into())).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        // Unknown conversation → 404.
        let (status, _) =
            body_json(conversation(State(state.clone()), Path("999999".into())).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // A conversation with a Spanish customer message reports es primary.
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO customers (remote_id, first_name, last_name) VALUES (901, 'Ada', 'Byron')",
            [],
        )
        .unwrap();
        let customer: i64 = conn
            .query_row("SELECT id FROM customers WHERE remote_id = 901", [], |r| {
                r.get(0)
            })
            .unwrap();
        let thread_remote = 5001;
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id,
                                        customer_id, created_at, closed_at, remote_created_at)
             VALUES (?1, 5001, 'Ayuda', 'closed', 1, ?2, datetime('now'), datetime('now'), datetime('now'))",
            rusqlite::params![thread_remote, customer],
        )
        .unwrap();
        let conv_id: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = ?1",
                rusqlite::params![thread_remote],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, thread_type, body, actor_type, state, created_at)
             VALUES (?1, 'customer', 'Hola, no puedo entrar en mi cuenta de usuario desde ayer', 'customer', 'published', datetime('now'))",
            rusqlite::params![conv_id],
        )
        .unwrap();
        drop(conn);
        let (status, body) =
            body_json(conversation(State(state.clone()), Path(conv_id.to_string())).await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["primary_language"]["code"], json!("es"));
        assert!(body["per_message"].as_array().unwrap().len() >= 1);
    }
}
