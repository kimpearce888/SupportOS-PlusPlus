//! Analytics routes — mirrors src/server/routes/analytics.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// GET /api/analytics/dashboard
///
/// Reference response shape:
/// ```json
/// {
///   "range": { "from": "...", "to": "..." },
///   "new_conversations": N, "active_conversations": N,
///   "pending_conversations": N, "closed_conversations": N,
///   "unassigned": N, "backlog": N,
///   "first_response_time_avg_min": null,
///   "resolution_time_avg_min": null,
///   "replies_sent": N,
///   "ratings": { "great": N, "okay": N, "not-good": N },
///   "by_mailbox": [], "by_tag": [], "by_agent": [], "by_team": [],
///   "daily_new": [], "by_channel": [], "channel_metrics": [],
///   "mailbox_comparison": [], "source": ["..."]
/// }
/// ```
pub async fn dashboard(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let mailbox_id = params.get("mailboxId").and_then(|m| m.parse::<i64>().ok());
    let days_back = params
        .get("daysBack")
        .and_then(|d| d.parse::<u32>().ok())
        .unwrap_or(7);
    let now = chrono::Utc::now();
    let from = now - chrono::Duration::days(days_back as i64);
    // Conversation status counts.
    let active: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE status = 'active'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let closed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE status = 'closed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let new_convs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE created_at >= ?1",
            rusqlite::params![from.to_rfc3339()],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let unassigned: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE assignee_id IS NULL AND status = 'active'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let backlog: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE status = 'active' AND created_at < ?1",
            rusqlite::params![from.to_rfc3339()],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let replies_sent: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversation_threads WHERE thread_type = 'reply'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // by_mailbox: count conversations per mailbox.
    let by_mailbox: Vec<Value> = conn
        .prepare(
            "SELECT mailbox_id, COUNT(*) FROM conversations GROUP BY mailbox_id ORDER BY 2 DESC",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "mailbox_id": r.get::<_, i64>(0)?,
                    "count": r.get::<_, i64>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let _ = mailbox_id;
    (
        StatusCode::OK,
        Json(json!({
            "range": {
                "from": from.to_rfc3339(),
                "to": now.to_rfc3339(),
            },
            "new_conversations": new_convs,
            "active_conversations": active,
            "pending_conversations": pending,
            "closed_conversations": closed,
            "unassigned": unassigned,
            "backlog": backlog,
            "first_response_time_avg_min": null,
            "resolution_time_avg_min": null,
            "replies_sent": replies_sent,
            "ratings": { "great": 0, "okay": 0, "not-good": 0 },
            "by_mailbox": by_mailbox,
            "by_tag": [],
            "by_agent": [],
            "by_team": [],
            "daily_new": [],
            "by_channel": [],
            "channel_metrics": [],
            "mailbox_comparison": [],
            "source": ["local"],
        })),
    )
}

/// GET /api/analytics/ai — AI run analytics.
///
/// Reference response shape:
/// ```json
/// {
///   "tickets_analyzed": N, "analysis_success_rate": N,
///   "draft_count": N, "draft_accepted": N, "draft_rejected": N,
///   "draft_edit_rate": N, "verification_warnings": N,
///   "unsupported_claim_rate": N, "common_failure_patterns": [],
///   "source": "local"
/// }
/// ```
pub async fn ai_analytics(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
        .query_row("SELECT COUNT(*) FROM verified_drafts", [], |r| r.get(0))
        .unwrap_or(0);
    let draft_accepted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM verified_drafts WHERE status = 'accepted'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let draft_rejected: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM verified_drafts WHERE status = 'rejected'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let analysis_success_rate = if total > 0 {
        (successful as f64) / (total as f64)
    } else {
        0.0
    };
    let draft_edit_rate = if draft_count > 0 {
        // We don't track edits separately; use 0.
        0.0
    } else {
        0.0
    };
    (
        StatusCode::OK,
        Json(json!({
            "tickets_analyzed": total,
            "analysis_success_rate": analysis_success_rate,
            "draft_count": draft_count,
            "draft_accepted": draft_accepted,
            "draft_rejected": draft_rejected,
            "draft_edit_rate": draft_edit_rate,
            "verification_warnings": 0,
            "unsupported_claim_rate": 0,
            "common_failure_patterns": [],
            "source": "local",
        })),
    )
}

/// GET /api/reports/sla
///
/// v1.4.0: SLA + business-hours report (per mailbox, business minutes).
/// Reference contract (routes/analytics.ts:48-62):
/// - `days` clamps through the shared helper (fallback 30, bounds 1-3650) —
///   `?days=abc` is a 200, not a 500 (v1.6.0 audit fix).
/// - `mailboxIds` is a comma-separated list of positive integers; any bad
///   token (including the empty tokens the reference does NOT filter here)
///   is a 422 with the exact reference message.
/// - `from`/`to` pass through verbatim when present; otherwise
///   `isoDaysAgo(days)` / now (`toISOString()` format).
pub async fn sla_report(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    // clampDaysParam(q.days, 30, 1, 3650)
    let days = clamp_days_param(params.get("days"), 30, 1, 3650);
    // mailboxIds: split(',').map(Number) — NO empty-token filtering on this
    // route (unlike /api/analytics/dashboard), so "1," is a 422.
    let mut mailbox_ids: Option<Vec<i64>> = None;
    if let Some(raw) = params.get("mailboxIds") {
        if !raw.is_empty() {
            let tokens: Vec<Option<f64>> = raw
                .split(',')
                .map(|t| crate::conversation_ops::js_number(t.trim()))
                .collect();
            if tokens
                .iter()
                .any(|id| id.is_none_or(|v| !v.is_finite() || v.fract() != 0.0 || v <= 0.0))
            {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "statusCode": 422,
                        "error": "ValidationError",
                        "message": "mailboxIds must be a comma-separated list of positive integers."
                    })),
                );
            }
            // Math.trunc(Number(id)) after validation — already integers.
            mailbox_ids = Some(
                tokens
                    .iter()
                    .map(|id| id.unwrap_or_default() as i64)
                    .collect(),
            );
        }
    }
    let from = params
        .get("from")
        .cloned()
        .unwrap_or_else(|| crate::business_hours::iso_days_ago(days));
    let to = params
        .get("to")
        .cloned()
        .unwrap_or_else(crate::business_hours::now_iso_millis);
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::sla::sla_report(&conn, &from, &to, mailbox_ids.as_deref()) {
        Ok(report) => (
            StatusCode::OK,
            Json(serde_json::to_value(&report).unwrap_or(Value::Null)),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// `clampDaysParam` (reference routes/helpers.ts): Number(value), NaN/garbage
/// falls back to the default, then clamps [min, max] after truncation.
fn clamp_days_param(raw: Option<&String>, fallback: i64, min: i64, max: i64) -> i64 {
    let Some(s) = raw.filter(|s| !s.is_empty()) else {
        return fallback;
    };
    match crate::conversation_ops::js_number(s) {
        Some(n) if n.is_finite() => (n.trunc() as i64).clamp(min, max),
        _ => fallback,
    }
}

pub async fn why_contacting(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"reasons": []})))
}
pub async fn top_questions(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"questions": []})))
}
pub async fn doc_gaps(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"gaps": []})))
}
pub async fn answer_reuse(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"reuse": []})))
}
pub async fn issue_radar(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::get_radar_snapshot(&conn) {
        Ok(snapshot) => {
            let alerts = vec![json!({
                "type": "radar_snapshot",
                "active_known_issues": snapshot.active_known_issues,
                "active_clusters": snapshot.active_clusters,
                "active_incidents": snapshot.active_incidents,
            })];
            (StatusCode::OK, Json(json!({"alerts": alerts})))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        ),
    }
}
pub async fn metric_definitions(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"definitions": []})))
}
pub async fn release_correlation(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"correlations": []})))
}
pub async fn release_events(
    State(state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"ok": true})))
}
pub async fn helpscout_report(
    State(state): State<AppState>,
    axum::extract::Path(_key): axum::extract::Path<String>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"data": []})))
}
pub async fn narrative(
    State(state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({"narrative": "Not implemented."})),
    )
}
/// GET /api/reports/effectiveness?days=90 — the observational response-style
/// report (reference `ctx.effectiveness.report(daysParam(q, 90))`).
pub async fn effectiveness(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let days = super::quality::clamp_days_param(params.get("days"), 90, 1, 3650);
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    (
        StatusCode::OK,
        Json(crate::quality::effectiveness_report(&conn, days)),
    )
}

/// GET /api/reports/builder/catalog — the closed metric/dimension catalog
/// (reference `ctx.reportBuilder.catalog()`): every entry ships its label,
/// definition and limitations; `origin: 'local'` labels every number.
pub async fn report_catalog() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "metrics": crate::catalog::reporting::all_metric_entries(),
            "dimensions": crate::catalog::reporting::all_dimension_entries(),
            "origin": "local",
            "note": "This builder computes local SupportOS metrics only. Native Help Scout reports remain available under the Help Scout tab, clearly labeled as Help Scout-originated."
        })),
    )
}

// ---------------- Report builder (reference analytics.ts builder routes) ----------------

/// POST /api/reports/builder/run — run a custom report.
///
/// Reference: `reportConfigSchema.safeParse` → 422 with joined
/// `path: message` issues; `reportBuilder.run(config)` result on success;
/// execution errors → 422 with the error message.
pub async fn builder_run(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    match crate::reports::parse_report_config(&body) {
        Err(issues) => {
            let message = issues
                .iter()
                .map(|(path, msg)| format!("{path}: {msg}"))
                .collect::<Vec<_>>()
                .join("; ");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": message
                })),
            )
        }
        Ok(config) => {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            match crate::reports::run_builder_report(&conn, &config) {
                Ok(outcome) => (
                    StatusCode::OK,
                    Json(serde_json::to_value(&outcome).unwrap_or(json!({}))),
                ),
                Err(message) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "statusCode": 422,
                        "error": "ValidationError",
                        "message": message
                    })),
                ),
            }
        }
    }
}

/// GET /api/reports/builder/saved — list saved report definitions.
pub async fn builder_saved_list(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::reports::list_saved_reports(&conn) {
        Ok(saved) => (
            StatusCode::OK,
            Json(json!({
                "saved": saved
                    .iter()
                    .filter_map(|s| serde_json::to_value(s).ok())
                    .collect::<Vec<Value>>()
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// POST /api/reports/builder/saved — save a report definition.
///
/// Reference: `{ name: z.string().min(1).max(120) }.and(reportConfigSchema)`
/// → parse errors surface as Zod 422 issues; `saveSaved(name, config)`.
pub async fn builder_saved_create(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    // Name validation first (reference: the intersection schema validates the
    // name AND the config together; name issues surface with path "name").
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() && n.len() <= 120 => n.to_string(),
        Some(n) if n.is_empty() => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": "name: String must contain at least 1 character(s)"
                })),
            );
        }
        Some(_) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": "name: String must contain at most 120 character(s)"
                })),
            );
        }
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": "name: Required"
                })),
            );
        }
    };
    match crate::reports::parse_report_config(&body) {
        Err(issues) => {
            let message = issues
                .iter()
                .map(|(path, msg)| format!("{path}: {msg}"))
                .collect::<Vec<_>>()
                .join("; ");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": message
                })),
            )
        }
        Ok(config) => {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            match crate::reports::save_report(&conn, &name, &config) {
                Ok(saved) => (
                    StatusCode::OK,
                    Json(json!({
                        "ok": true,
                        "saved": serde_json::to_value(&saved).unwrap_or(json!({}))
                    })),
                ),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message": e.to_string()})),
                ),
            }
        }
    }
}

/// DELETE /api/reports/builder/saved/:id — delete a saved report.
pub async fn builder_saved_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let id_num = crate::conversation_ops::js_number(&id);
    let Some(idv) = id_num.filter(|v| v.fract() == 0.0 && *v > 0.0) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "Report id must be a positive integer."
            })),
        );
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::reports::delete_saved_report(&conn, idv as i64) {
        Ok(true) => (StatusCode::OK, Json(json!({"ok": true}))),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Saved report not found."
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::{IntoResponse, Response};
    use std::collections::HashMap;
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

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn seed_mailbox_and_pair(state: &AppState) {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, mailbox_id, customer_id, status, created_at, updated_at)
             VALUES (101, 101, 1, 3001, 'closed', '2026-03-02T09:00:00Z', '2026-03-02T10:00:00Z')",
            [],
        )
        .unwrap();
        let conv: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 101",
                [],
                |r| r.get(0),
            )
            .unwrap();
        for (kind, at) in [
            ("customer", "2026-03-02T09:00:00Z"),
            ("reply", "2026-03-02T09:30:00Z"),
        ] {
            conn.execute(
                "INSERT INTO conversation_threads (conversation_id, thread_type, state, body, actor_type, created_at)
                 VALUES (?1, ?2, 'published', 'b', 'customer', ?3)",
                rusqlite::params![conv, kind, at],
            )
            .unwrap();
        }
    }

    /// Reference e2e (v14): the report works before any schedule is
    /// configured — wall minutes present, business minutes null, mailbox
    /// unconfigured, first-response count > 0.
    #[tokio::test]
    async fn sla_report_wall_clock_honest_before_configuration() {
        let state = make_state();
        seed_mailbox_and_pair(&state);
        let (status, body) = body_json(
            sla_report(
                State(state),
                Query(params(&[
                    ("from", "2026-03-01T00:00:00.000Z"),
                    ("to", "2026-03-31T23:59:59.999Z"),
                ])),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let mailboxes = body["mailboxes"].as_array().unwrap();
        assert_eq!(mailboxes.len(), 1);
        let m = &mailboxes[0];
        assert_eq!(m["mailbox_name"], json!("Support"));
        assert_eq!(m["business_hours_configured"], json!(false));
        assert_eq!(m["first_response"]["count"], json!(1));
        assert_eq!(m["first_response"]["avg_wall_min"], json!(30));
        assert_eq!(m["first_response"]["avg_business_min"], Value::Null);
        assert_eq!(body["unconfigured_mailboxes"], json!(["Support"]));
        assert_eq!(body["source"], json!(["local"]));
        // range round-trips the computed window (toISOString format).
        assert!(
            body["range"]["from"].as_str().unwrap().ends_with('Z'),
            "from: {}",
            body["range"]["from"]
        );
    }

    /// Reference e2e (v14): invalid mailboxIds is a 422 with the exact
    /// message; the empty token "1," is ALSO invalid on this route (the
    /// reference does not filter empty tokens here, unlike dashboard).
    #[tokio::test]
    async fn sla_report_invalid_mailbox_ids_is_422() {
        for raw in ["abc", "1,", "1.5", "0", "-2"] {
            let state = make_state();
            let (status, body) = body_json(
                sla_report(State(state), Query(params(&[("mailboxIds", raw)])))
                    .await
                    .into_response(),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "raw: {raw}");
            assert_eq!(
                body["message"],
                json!("mailboxIds must be a comma-separated list of positive integers.")
            );
        }
    }

    /// Reference e2e (audit-v16): `?days=abc` clamps to the fallback (30) and
    /// answers 200 — never a 500.
    #[tokio::test]
    async fn sla_report_days_abc_is_200() {
        let state = make_state();
        let (status, _body) = body_json(
            sla_report(State(state), Query(params(&[("days", "abc")])))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// Valid mailboxIds scopes the report; the exact reference shape
    /// (schedule block with startMinute/endMinute) appears after
    /// configuration.
    #[tokio::test]
    async fn sla_report_scopes_and_exposes_schedule() {
        let state = make_state();
        seed_mailbox_and_pair(&state);
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO mailbox_business_hours
                    (mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min, updated_at)
                 VALUES (1, 'UTC', '[0,1,2,3,4,5,6]', 0, 1440, 60, 4320, '2026-01-01T00:00:00.000Z')",
                [],
            )
            .unwrap();
            // A second mailbox stays unconfigured.
            conn.execute(
                "INSERT INTO mailboxes (id, remote_id, name) VALUES (2, 202, 'Billing')",
                [],
            )
            .unwrap();
        }
        let (status, body) = body_json(
            sla_report(
                State(state),
                Query(params(&[
                    ("mailboxIds", "1"),
                    ("from", "2026-03-01T00:00:00.000Z"),
                    ("to", "2026-03-31T23:59:59.999Z"),
                ])),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let mailboxes = body["mailboxes"].as_array().unwrap();
        assert_eq!(mailboxes.len(), 1, "scoped to mailbox 1");
        let m = &mailboxes[0];
        assert_eq!(m["business_hours_configured"], json!(true));
        assert_eq!(m["schedule"]["timezone"], json!("UTC"));
        assert_eq!(m["schedule"]["startMinute"], json!(0));
        assert_eq!(m["schedule"]["endMinute"], json!(1440));
        assert_eq!(m["first_response"]["target_min"], json!(60));
        // 24/7 schedule: business == wall (reference e2e assertion).
        assert_eq!(
            m["first_response"]["avg_business_min"],
            m["first_response"]["avg_wall_min"]
        );
        assert_eq!(m["first_response"]["avg_business_min"], json!(30));
    }
}
