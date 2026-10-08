//! Attributes routes — mirrors src/server/routes/attributes.ts
//!
//! General AI Attribute Layer (v1.9.0 / M3, plan Phase 16): snapshot per
//! conversation, version history, searchable conversation lists, aggregate
//! distributions (reportable), distinct filter values, and manual recompute.
//! The catalog endpoint powers every filter UI (Views / automation /
//! segments). A missing attribute reads as unknown — it is never fabricated.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::super::server::AppState;
use crate::ai_attributes::{self, MatchOp};
use crate::catalog::{AiAttributeKey, AttributeValueType, AI_ATTRIBUTE_SCHEMA_VERSION};

/// The reference `clampListParam`: Number(raw) with NaN → fallback, then
/// clamp to [min, max] after truncation.
fn clamp_list_param(raw: Option<&String>, fallback: i64, min: i64, max: i64) -> i64 {
    let n = match raw {
        Some(s) if !s.is_empty() => crate::conversation_ops::js_number(s),
        _ => Some(fallback as f64),
    };
    match n {
        Some(v) if v.is_finite() => (v.trunc() as i64).clamp(min, max),
        _ => fallback,
    }
}

/// JavaScript `Number(params.id)` + `Number.isInteger(id) && id > 0`
/// validation; `None` = invalid.
fn parse_positive_int(raw: &str) -> Option<i64> {
    let n = crate::conversation_ops::js_number(raw)?;
    if n.fract() != 0.0 || n <= 0.0 || n > i64::MAX as f64 {
        return None;
    }
    Some(n as i64)
}

/// The reference route-level 422 envelope (no issues array).
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

/// The reference route-level 404 envelope.
fn not_found_404() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": "Conversation not found.",
        })),
    )
        .into_response()
}

/// GET /api/attributes/catalog — the closed 14-key catalog.
pub async fn catalog(State(state): State<AppState>) -> Response {
    let _ = state; // the catalog is static (single source of truth)
    let catalog: Vec<Value> = AiAttributeKey::ALL
        .iter()
        .map(|k| {
            let mut entry = json!({
                "key": k.as_str(),
                "label": k.label(),
                "value_type": k.value_type().as_str(),
                "description": k.description(),
            });
            if k.value_type() == AttributeValueType::Enum {
                entry["values"] = json!(k.values());
            }
            entry
        })
        .collect();
    (
        StatusCode::OK,
        Json(json!({
            "catalog": catalog,
            "schema_version": AI_ATTRIBUTE_SCHEMA_VERSION,
            "note": "A missing attribute reads as unknown - it is never fabricated. \
                     Attributes are local-only and never written to Help Scout."
        })),
    )
        .into_response()
}

/// GET /api/attributes/conversation/:id — the current snapshot.
pub async fn conversation_attributes(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Some(id) = parse_positive_int(&id) else {
        return validation_422("Conversation id must be a positive integer.");
    };
    let conn = state.conn_lock();
    match ai_attributes::snapshot(&conn, id) {
        Ok(Some(snapshot)) => (StatusCode::OK, Json(snapshot)).into_response(),
        Ok(None) => not_found_404(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "statusCode": 503,
                "error": "ServiceUnavailable",
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

/// GET /api/attributes/conversation/:id/history/:attribute — full version
/// history for one attribute (newest first).
pub async fn conversation_attribute_history(
    State(state): State<AppState>,
    Path((id, attribute)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = parse_positive_int(&id) else {
        return validation_422("Conversation id must be a positive integer.");
    };
    let Some(key) = AiAttributeKey::parse(&attribute) else {
        return validation_422(&format!(
            "Unknown attribute '{attribute}'. Use /api/attributes/catalog for the closed list."
        ));
    };
    let limit = clamp_list_param(params.get("limit"), 50, 1, 200);
    let conn = state.conn_lock();
    match ai_attributes::history(&conn, id, key, limit) {
        Ok(history) => (StatusCode::OK, Json(json!({ "history": history }))).into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "statusCode": 503,
                "error": "ServiceUnavailable",
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

/// GET /api/attributes/conversations — conversations matching an attribute
/// test (drill-down lists). Unknown attributes return an empty 200 with a
/// note, never an error.
pub async fn conversations(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let attribute = params.get("attribute").cloned().unwrap_or_default();
    let Some(key) = AiAttributeKey::parse(&attribute) else {
        return (
            StatusCode::OK,
            Json(json!({
                "attribute": attribute,
                "op": null,
                "value": null,
                "conversations": [],
                "note": format!(
                    "Unknown attribute '{attribute}'. Use /api/attributes/catalog."
                ),
            })),
        )
            .into_response();
    };
    let op_raw = params.get("op").cloned().unwrap_or_else(|| "equals".into());
    let op = MatchOp::parse(&op_raw, key.value_type());
    let value: String = params
        .get("value")
        .cloned()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect();
    let limit = clamp_list_param(params.get("limit"), 25, 1, 200);
    let conn = state.conn_lock();
    let conversations =
        ai_attributes::conversations_matching(&conn, key, op, &value, limit).unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({
            "attribute": attribute,
            "op": op.as_str(),
            "value": value,
            "conversations": conversations,
        })),
    )
        .into_response()
}

/// GET /api/attributes/report — distribution + honest coverage per attribute.
pub async fn report(State(state): State<AppState>) -> Response {
    let conn = state.conn_lock();
    let distributions = ai_attributes::distributions(&conn).unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({ "distributions": distributions })),
    )
        .into_response()
}

/// GET /api/attributes/values/:attribute — distinct current values for
/// filter autocomplete.
pub async fn values(
    State(state): State<AppState>,
    Path(attribute): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(key) = AiAttributeKey::parse(&attribute) else {
        return validation_422(&format!("Unknown attribute '{attribute}'."));
    };
    let limit = clamp_list_param(params.get("limit"), 50, 1, 200);
    let conn = state.conn_lock();
    let values = ai_attributes::distinct_values(&conn, key, limit).unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({ "attribute": attribute, "values": values })),
    )
        .into_response()
}

/// POST /api/attributes/conversation/:id/recompute — manual recompute
/// (deterministic always; AI when enabled + reachable).
pub async fn recompute(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let Some(id) = parse_positive_int(&id) else {
        return validation_422("Conversation id must be a positive integer.");
    };
    // recomputeSchema: z.object({ force: z.boolean().optional() }).default({})
    // — a missing key reads as undefined (optional passes); `null` is NOT
    // accepted (Zod's .optional() allows undefined only), and the received
    // type is named exactly like Zod's invalid_type message.
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let force = match body.get("force") {
        None => false,
        Some(v) if v.is_boolean() => v.as_bool().unwrap_or(false),
        Some(v) => {
            return crate::conversation_ops::zod_422(
                "force",
                &format!("Expected boolean, received {}", zod_received(v)),
            );
        }
    };

    // Provider selection (reference ctx provider: LM Studio when configured).
    let (model, use_ai, base_url, timeout_ms) = {
        let conn = state.conn_lock();
        let status = crate::ai_center::get_ai_status(&conn).unwrap_or_default();
        let model = status.chat_model.clone().unwrap_or_default();
        let use_ai = crate::ai_center::ai_features_enabled(&status);
        // AI-22: the configured base URL + lmstudio_timeout_ms (the old
        // code ignored both and always dialed the default URL unbounded).
        let base_url = crate::settings::get_string(&conn, "lmstudio_base_url")
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .or_else(|| status.base_url.clone())
            .unwrap_or_else(|| crate::ai_lm_studio::LM_STUDIO_BASE_URL.to_string());
        let timeout_ms = u64::try_from(
            crate::settings::get_i64(&conn, "lmstudio_timeout_ms", 120_000).unwrap_or(120_000),
        )
        .unwrap_or(crate::ai_lm_studio::LM_STUDIO_DEFAULT_TIMEOUT_MS);
        (model, use_ai, base_url, timeout_ms)
    };
    let provider: Box<dyn crate::ai_provider::LocalAiProvider> = if use_ai {
        Box::new(
            crate::ai_lm_studio::LmStudioProvider::with_base_url_and_timeout(base_url, timeout_ms),
        )
    } else {
        Box::new(crate::ai_provider::NoopAiProvider)
    };

    // Phase 1: deterministic layer + prompt/cache (connection released after).
    let prepared = {
        let conn = state.conn_lock();
        match ai_attributes::prepare_computation(&conn, id, &model, force) {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return not_found_404(),
            Err(e) => return service_unavailable(e.to_string()),
        }
    };

    // Phase 2: the await-able provider call — NO connection held.
    let fresh = match (&prepared.prompt, prepared.cached.is_none()) {
        (Some(prompt), true) => {
            ai_attributes::run_extraction(provider.as_ref(), &model, prompt).await
        }
        _ => None,
    };

    // Phase 3: merge, persist, audit.
    let result = {
        let mut conn = state.conn_lock();
        let snapshot = match ai_attributes::finish_computation(&mut conn, prepared, fresh, &model) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return not_found_404(),
            Err(e) => return service_unavailable(e.to_string()),
        };
        // Audit record (reference: actor 'user', action
        // 'attributes_recomputed', ai_involvement true).
        let _ = crate::audit::audit(
            &conn,
            &crate::audit::AuditEntry {
                actor: "user",
                action: "attributes_recomputed".into(),
                conversation_id: Some(id),
                before_state: None,
                after_state: None,
                remote_operation: None,
                remote_result: None,
                ai_involvement: true,
                job_id: None,
                correlation_id: None,
            },
        );
        snapshot
    };

    (
        StatusCode::OK,
        Json(json!({ "ok": true, "snapshot": result })),
    )
        .into_response()
}

/// The reference 503 envelope.
fn service_unavailable(message: String) -> Response {
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

/// Zod v3 `parsedType` names for the JSON value kinds (used in the
/// invalid_type error message).
fn zod_received(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn make_state() -> (AppState, Arc<Mutex<Connection>>) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (
                id INTEGER PRIMARY KEY, remote_id INTEGER NOT NULL UNIQUE,
                number INTEGER, subject TEXT, status TEXT NOT NULL DEFAULT 'active',
                mailbox_id INTEGER NOT NULL, customer_id INTEGER NOT NULL);
             CREATE TABLE conversation_threads (
                id INTEGER PRIMARY KEY AUTOINCREMENT, conversation_id INTEGER NOT NULL,
                thread_type TEXT NOT NULL, body TEXT, actor_type TEXT NOT NULL,
                actor_id INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE known_issues (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
             CREATE TABLE known_issue_links (
                id INTEGER PRIMARY KEY AUTOINCREMENT, known_issue_id INTEGER NOT NULL,
                conversation_id INTEGER NOT NULL, link_type TEXT NOT NULL DEFAULT 'related');
             CREATE TABLE issue_clusters (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
             CREATE TABLE issue_cluster_conversations (
                cluster_id INTEGER NOT NULL, conversation_id INTEGER NOT NULL,
                PRIMARY KEY (cluster_id, conversation_id));",
        )
        .unwrap();
        crate::embeddings::apply_m008(&conn).unwrap();
        ai_attributes::apply_m033(&conn).unwrap();
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

    fn insert_conversation(conn: &Connection, id: i64, number: i64, subject: &str) {
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id)
             VALUES (?1, ?1, ?2, ?3, 1, 1)",
            rusqlite::params![id, number, subject],
        )
        .unwrap();
    }

    async fn body_json(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    fn query(pairs: &[(&str, &str)]) -> Query<HashMap<String, String>> {
        let mut map = HashMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_string(), (*v).to_string());
        }
        Query(map)
    }

    #[tokio::test]
    async fn catalog_lists_14_keys_with_schema_version() {
        let (state, _conn) = make_state();
        let (status, body) = body_json(catalog(State(state)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["schema_version"], "attributes_v1");
        let catalog = body["catalog"].as_array().unwrap();
        assert_eq!(catalog.len(), 14);
        let intent = catalog
            .iter()
            .find(|d| d["key"] == "intent")
            .expect("intent present");
        assert_eq!(intent["label"], "Intent");
        assert_eq!(intent["value_type"], "enum");
        assert!(intent["values"].is_array());
        let product = catalog
            .iter()
            .find(|d| d["key"] == "product")
            .expect("product present");
        assert_eq!(product["value_type"], "text");
        assert!(product.get("values").is_none());
        assert!(body["note"].as_str().unwrap().contains("never fabricated"));
    }

    #[tokio::test]
    async fn conversation_id_validation_and_404() {
        let (state, _conn) = make_state();
        for bad in ["0", "-3", "abc", "3.5"] {
            let (status, body) = body_json(
                conversation_attributes(State(state.clone()), Path(bad.to_string())).await,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
            assert_eq!(body["statusCode"], 422);
            assert_eq!(body["error"], "ValidationError");
            assert_eq!(
                body["message"],
                "Conversation id must be a positive integer."
            );
        }
        let (status, body) =
            body_json(conversation_attributes(State(state), Path("999".to_string())).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["statusCode"], 404);
        assert_eq!(body["error"], "NotFound");
        assert_eq!(body["message"], "Conversation not found.");
    }

    #[tokio::test]
    async fn conversation_snapshot_shape() {
        let (state, conn) = make_state();
        {
            let mut c = conn.lock().unwrap();
            insert_conversation(&c, 1, 101, "Ticket 101");
            ai_attributes::save_snapshot(
                &mut c,
                1,
                &[ai_attributes::AttributeRecord {
                    key: AiAttributeKey::Urgency,
                    value: "high".into(),
                    confidence: "medium",
                    source: "deterministic",
                    evidence: vec![json!({ "excerpt": "this is urgent", "thread_local_id": 1 })],
                    run_id: None,
                }],
                None,
            )
            .unwrap();
        }
        let (status, body) =
            body_json(conversation_attributes(State(state), Path("1".to_string())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["conversation_id"], 1);
        assert_eq!(body["conversation_number"], 101);
        let attrs = body["attributes"].as_array().unwrap();
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0]["attribute"], "urgency");
        assert_eq!(attrs[0]["value"], "high");
        assert_eq!(attrs[0]["confidence"], "medium");
        assert_eq!(attrs[0]["source"], "deterministic");
        assert_eq!(attrs[0]["schema_version"], "attributes_v1");
        assert_eq!(attrs[0]["status"], "known");
        assert!(attrs[0]["computed_at"].is_string());
        assert_eq!(attrs[0]["evidence"][0]["excerpt"], "this is urgent");
        assert!(body["unknown"]
            .as_array()
            .unwrap()
            .contains(&json!("intent")));
    }

    #[tokio::test]
    async fn history_validates_attribute_and_id() {
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            insert_conversation(&c, 1, 101, "T");
        }
        let (status, body) = body_json(
            conversation_attribute_history(
                State(state.clone()),
                Path(("1".to_string(), "not_a_key".to_string())),
                query(&[]),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            "Unknown attribute 'not_a_key'. Use /api/attributes/catalog for the closed list."
        );
        let (status, _body) = body_json(
            conversation_attribute_history(
                State(state.clone()),
                Path(("x".to_string(), "urgency".to_string())),
                query(&[]),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let (status, body) = body_json(
            conversation_attribute_history(
                State(state),
                Path(("1".to_string(), "urgency".to_string())),
                query(&[("limit", "10")]),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["history"].is_array());
    }

    #[tokio::test]
    async fn conversations_unknown_attribute_returns_200_with_note() {
        let (state, _conn) = make_state();
        let (status, body) =
            body_json(conversations(State(state), query(&[("attribute", "bogus")])).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["attribute"], "bogus");
        assert!(body["op"].is_null());
        assert!(body["value"].is_null());
        assert_eq!(body["conversations"].as_array().unwrap().len(), 0);
        assert_eq!(
            body["note"],
            "Unknown attribute 'bogus'. Use /api/attributes/catalog."
        );
    }

    #[tokio::test]
    async fn conversations_matching_with_op_allowlist() {
        let (state, conn) = make_state();
        {
            let mut c = conn.lock().unwrap();
            insert_conversation(&c, 1, 101, "T1");
            ai_attributes::save_snapshot(
                &mut c,
                1,
                &[
                    ai_attributes::AttributeRecord {
                        key: AiAttributeKey::Product,
                        value: "Payments".into(),
                        confidence: "medium",
                        source: "ai",
                        evidence: vec![],
                        run_id: None,
                    },
                    ai_attributes::AttributeRecord {
                        key: AiAttributeKey::QuestionCount,
                        value: "3".into(),
                        confidence: "high",
                        source: "deterministic",
                        evidence: vec![],
                        run_id: None,
                    },
                ],
                None,
            )
            .unwrap();
        }
        // equals (case-insensitive).
        let (status, body) = body_json(
            conversations(
                State(state.clone()),
                query(&[
                    ("attribute", "product"),
                    ("op", "equals"),
                    ("value", "payments"),
                ]),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["op"], "equals");
        let hits = body["conversations"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["conversation_id"], 1);
        assert_eq!(hits[0]["number"], 101);
        assert_eq!(hits[0]["subject"], "T1");
        // invalid op falls back to equals.
        let (_status, body) = body_json(
            conversations(
                State(state.clone()),
                query(&[
                    ("attribute", "product"),
                    ("op", "bogus"),
                    ("value", "payments"),
                ]),
            )
            .await,
        )
        .await;
        assert_eq!(body["op"], "equals");
        // 'contains' is not allowed for numbers → falls back to equals.
        let (_status, body) = body_json(
            conversations(
                State(state.clone()),
                query(&[
                    ("attribute", "question_count"),
                    ("op", "contains"),
                    ("value", "3"),
                ]),
            )
            .await,
        )
        .await;
        assert_eq!(body["op"], "equals");
        assert_eq!(body["conversations"].as_array().unwrap().len(), 1);
        // gt is allowed for numbers.
        let (_status, body) = body_json(
            conversations(
                State(state.clone()),
                query(&[
                    ("attribute", "question_count"),
                    ("op", "gt"),
                    ("value", "2"),
                ]),
            )
            .await,
        )
        .await;
        assert_eq!(body["op"], "gt");
        assert_eq!(body["conversations"].as_array().unwrap().len(), 1);
        // unknown op lists conversations without a current row.
        let (_status, body) = body_json(
            conversations(
                State(state),
                query(&[("attribute", "intent"), ("op", "unknown")]),
            )
            .await,
        )
        .await;
        let hits = body["conversations"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["confidence"], "unknown");
        assert_eq!(hits[0]["source"], "none");
    }

    #[tokio::test]
    async fn report_lists_distributions() {
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            insert_conversation(&c, 1, 101, "T1");
        }
        let (status, body) = body_json(report(State(state)).await).await;
        assert_eq!(status, StatusCode::OK);
        let dists = body["distributions"].as_array().unwrap();
        assert_eq!(dists.len(), 14);
        assert_eq!(dists[0]["total_conversations"], 1);
        assert_eq!(dists[0]["unknown"], 1);
        assert_eq!(dists[0]["known"], 0);
    }

    #[tokio::test]
    async fn values_validates_attribute_and_lists_distinct() {
        let (state, conn) = make_state();
        {
            let mut c = conn.lock().unwrap();
            insert_conversation(&c, 1, 101, "T1");
            ai_attributes::save_snapshot(
                &mut c,
                1,
                &[ai_attributes::AttributeRecord {
                    key: AiAttributeKey::Risk,
                    value: "low".into(),
                    confidence: "low",
                    source: "deterministic",
                    evidence: vec![],
                    run_id: None,
                }],
                None,
            )
            .unwrap();
        }
        let (status, body) =
            body_json(values(State(state.clone()), Path("bogus".to_string()), query(&[])).await)
                .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["message"], "Unknown attribute 'bogus'.");

        let (status, body) =
            body_json(values(State(state), Path("risk".to_string()), query(&[])).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["attribute"], "risk");
        assert_eq!(body["values"], json!(["low"]));
    }

    #[tokio::test]
    async fn recompute_validates_id_and_missing_conversation() {
        let (state, _conn) = make_state();
        let (status, body) = body_json(
            recompute(
                State(state.clone()),
                Path("0".to_string()),
                Some(Json(json!({}))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            "Conversation id must be a positive integer."
        );

        let (status, body) = body_json(
            recompute(State(state), Path("999".to_string()), Some(Json(json!({})))).await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["message"], "Conversation not found.");
    }

    #[tokio::test]
    async fn recompute_force_must_be_boolean() {
        let (state, _conn) = make_state();
        // Zod: `null` is not accepted by z.boolean().optional().
        let (status, body) = body_json(
            recompute(
                State(state.clone()),
                Path("1".to_string()),
                Some(Json(json!({ "force": null }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            "Invalid request (force): Expected boolean, received null"
        );
        assert_eq!(
            body["issues"][0]["message"],
            "Expected boolean, received null"
        );

        let (status, body) = body_json(
            recompute(
                State(state),
                Path("1".to_string()),
                Some(Json(json!({ "force": "yes" }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            "Invalid request (force): Expected boolean, received string"
        );
    }

    #[tokio::test]
    async fn recompute_persists_deterministic_snapshot_and_audits() {
        let (state, conn) = make_state();
        {
            let c = conn.lock().unwrap();
            insert_conversation(&c, 1, 101, "Urgent bug");
            c.execute(
                "INSERT INTO conversation_threads (conversation_id, thread_type, body, actor_type)
                 VALUES (1, 'customer', 'This is urgent, production is down! When will this be fixed?', 'customer')",
                [],
            )
            .unwrap();
        }
        // No AI settings row → provider disabled → deterministic-only.
        let (status, body) = body_json(
            recompute(
                State(state.clone()),
                Path("1".to_string()),
                Some(Json(json!({ "force": false }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        let snapshot = &body["snapshot"];
        assert_eq!(snapshot["conversation_id"], 1);
        assert_eq!(snapshot["attributes"].as_array().unwrap().len() > 0, true);
        assert!(snapshot["unknown"]
            .as_array()
            .unwrap()
            .contains(&json!("intent")));

        // Audit record written with ai_involvement.
        {
            let c = conn.lock().unwrap();
            let (action, ai_involvement): (String, i64) = c
                .query_row(
                    "SELECT action, ai_involvement FROM audit_log
                     WHERE action = 'attributes_recomputed' ORDER BY id DESC LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("audit row present");
            assert_eq!(action, "attributes_recomputed");
            assert_eq!(ai_involvement, 1);
        }

        // Snapshot now readable via the GET route.
        let (status, body) =
            body_json(conversation_attributes(State(state), Path("1".to_string())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["attributes"].as_array().unwrap().len() > 0, true);
    }

    #[tokio::test]
    async fn limit_clamping_follows_the_reference() {
        assert_eq!(clamp_list_param(None, 50, 1, 200), 50);
        assert_eq!(clamp_list_param(Some(&"abc".to_string()), 50, 1, 200), 50);
        assert_eq!(clamp_list_param(Some(&"".to_string()), 50, 1, 200), 50);
        assert_eq!(clamp_list_param(Some(&"0".to_string()), 50, 1, 200), 1);
        assert_eq!(clamp_list_param(Some(&"9999".to_string()), 50, 1, 200), 200);
        assert_eq!(clamp_list_param(Some(&"7.9".to_string()), 50, 1, 200), 7);
        assert_eq!(clamp_list_param(Some(&"-5".to_string()), 50, 1, 200), 1);
    }

    #[test]
    fn positive_int_validation_follows_js_number() {
        assert_eq!(parse_positive_int("3"), Some(3));
        assert_eq!(parse_positive_int(" 3 "), Some(3));
        assert_eq!(parse_positive_int("3.0"), Some(3));
        assert_eq!(parse_positive_int("3.5"), None);
        assert_eq!(parse_positive_int("0"), None);
        assert_eq!(parse_positive_int("-2"), None);
        assert_eq!(parse_positive_int("abc"), None);
        assert_eq!(parse_positive_int(""), None);
    }
}
