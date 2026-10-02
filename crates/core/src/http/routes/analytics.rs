//! Analytics routes — mirrors src/server/routes/analytics.ts

use axum::extract::{Query, State};
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
    }))
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
    }))
}

/// GET /api/reports/sla
///
/// Reference response shape:
/// ```json
/// {
///   "range": { "from": "...", "to": "..." },
///   "mailboxes": [],
///   "unconfigured_mailboxes": [],
///   "source": ["..."]
/// }
/// ```
pub async fn sla_report(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let days_back = params
        .get("daysBack")
        .and_then(|d| d.parse::<u32>().ok())
        .unwrap_or(7);
    let now = chrono::Utc::now();
    let from = now - chrono::Duration::days(days_back as i64);
    // Pull mailboxes that have SLA configs configured.
    let mailboxes: Vec<Value> = conn
        .prepare("SELECT mailbox_id, config_json FROM sla_configs ORDER BY mailbox_id")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "mailbox_id": r.get::<_, i64>(0)?,
                    "config": r.get::<_, String>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    // Unconfigured mailboxes: those that have conversations but no SLA config.
    let unconfigured_mailboxes: Vec<Value> = conn
        .prepare("SELECT DISTINCT m.id, m.name FROM mailboxes m LEFT JOIN sla_configs s ON s.mailbox_id = m.id WHERE s.mailbox_id IS NULL ORDER BY m.id")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "range": {
            "from": from.to_rfc3339(),
            "to": now.to_rfc3339(),
        },
        "mailboxes": mailboxes,
        "unconfigured_mailboxes": unconfigured_mailboxes,
        "source": ["local"],
    }))
}

pub async fn why_contacting(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"reasons": []}))
}
pub async fn top_questions(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"questions": []}))
}
pub async fn doc_gaps(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"gaps": []}))
}
pub async fn answer_reuse(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"reuse": []}))
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
            Json(json!({"alerts": alerts}))
        }
        Err(e) => Json(json!({"_status": 500, "message": e.to_string()})),
    }
}
pub async fn metric_definitions(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"definitions": []}))
}
pub async fn release_correlation(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"correlations": []}))
}
pub async fn release_events(
    State(state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    Json(json!({"ok": true}))
}
pub async fn helpscout_report(
    State(state): State<AppState>,
    axum::extract::Path(_key): axum::extract::Path<String>,
) -> impl IntoResponse {
    Json(json!({"data": []}))
}
pub async fn narrative(
    State(state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    Json(json!({"narrative": "Not implemented."}))
}
pub async fn effectiveness(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"effectiveness": []}))
}
pub async fn report_catalog(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::reports::get_health_facts(&conn, 7) {
        Ok(facts) => Json(serde_json::to_value(&facts).unwrap_or(json!({}))),
        Err(e) => Json(json!({"_status": 500, "message": e.to_string()})),
    }
}
