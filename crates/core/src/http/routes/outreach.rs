//! Outreach routes — mirrors src/server/routes/outreach.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/outreach/meta
pub async fn meta(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let segments: i64 = conn
        .query_row("SELECT COUNT(*) FROM saved_segments", [], |r| r.get(0))
        .unwrap_or(0);
    let campaigns: i64 = conn
        .query_row("SELECT COUNT(*) FROM campaigns", [], |r| r.get(0))
        .unwrap_or(0);
    let dnc: i64 = conn
        .query_row("SELECT COUNT(*) FROM do_not_contact", [], |r| r.get(0))
        .unwrap_or(0);
    Json(json!({"segments": segments, "campaigns": campaigns, "dnc": dnc}))
}

/// POST /api/outreach/segments/preview
pub async fn preview_segment(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"matches": [], "count": 0}))
}

/// POST /api/outreach/segments/estimate
pub async fn estimate_segment(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"estimate": 0}))
}

/// POST /api/outreach/segments/suggest
pub async fn suggest_segment(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"suggestions": []}))
}

/// GET /api/outreach/segments
pub async fn list_segments(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let segments = crate::outreach::list_segments(&conn).unwrap_or_default();
    let items: Vec<Value> = segments
        .iter()
        .filter_map(|s| serde_json::to_value(s).ok())
        .collect();
    Json(json!({"segments": items}))
}

/// POST /api/outreach/segments
pub async fn create_segment(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let criteria = body.get("criteria").and_then(|v| v.as_str()).unwrap_or("");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::outreach::create_segment(&conn, name, criteria) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// DELETE /api/outreach/segments/:id
pub async fn delete_segment(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM saved_segments WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/outreach/campaigns
pub async fn create_campaign(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let segment_id = body.get("segmentId").and_then(|v| v.as_i64());
    let template = body
        .get("messageTemplate")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::outreach::create_campaign(&conn, name, segment_id, Some(template)) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// GET /api/outreach/campaigns
pub async fn list_campaigns(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let campaigns = crate::outreach::list_campaigns(&conn).unwrap_or_default();
    let items: Vec<Value> = campaigns
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    Json(json!({"campaigns": items}))
}

/// GET /api/outreach/campaigns/:id
pub async fn get_campaign(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let campaigns = crate::outreach::list_campaigns(&conn).unwrap_or_default();
    if let Some(campaign) = campaigns.iter().find(|c| c.id == Some(id)) {
        Json(serde_json::to_value(campaign).unwrap_or(json!({})))
    } else {
        Json(json!({"error": "Campaign not found"}))
    }
}

// ---------------------------------------------------------------------------
// Campaign lifecycle + DNC (reference outreach.ts:280-405 — campaignService
// port). All handlers return the reference's exact response shapes.
// ---------------------------------------------------------------------------

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// GET /api/outreach/campaigns/:id/validate
pub async fn campaign_validate_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return campaign_not_found();
    };
    let conn = state.conn_lock();
    Json(crate::outreach::campaign_validate(&conn, id)).into_response()
}

/// POST /api/outreach/render — campaign-less personalization preview.
pub async fn render_route(State(state): State<AppState>, body: Option<Json<Value>>) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let customer_id = match body.get("customer_local_id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422, "error": "ValidationError",
                    "message": "customer_local_id is required."
                })),
            )
                .into_response()
        }
    };
    let conn = state.conn_lock();
    let customer: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT first_name, last_name, email FROM customers WHERE id = ?1",
            [customer_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((first_name, last_name, email)) = customer else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404, "error": "NotFound",
                "message": "Customer not found in the local mirror."
            })),
        )
            .into_response();
    };
    // Matching tickets: caller-provided or the customer's 3 most recent.
    let tickets: Vec<Value> = match body.get("matching_tickets").and_then(|v| v.as_array()) {
        Some(t) => t.clone(),
        None => conn
            .prepare(
                "SELECT c.id, c.number, c.subject, c.status, c.created_at,
                        (SELECT GROUP_CONCAT(t.name) FROM conversation_tags ct
                          JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id)
                   FROM conversations c WHERE c.customer_id = ?1
                  ORDER BY c.created_at DESC LIMIT 3",
            )
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map([customer_id], |r| {
                    let tags_raw: Option<String> = r.get(5)?;
                    let tags: Vec<String> = tags_raw
                        .map(|t| {
                            t.split(',')
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    Ok(json!({
                        "conversationId": r.get::<_, i64>(0)?,
                        "number": r.get::<_, i64>(1)?,
                        "subject": r.get::<_, Option<String>>(2)?,
                        "status": r.get::<_, String>(3)?,
                        "createdAt": r.get::<_, Option<String>>(4)?,
                        "tags": tags,
                    }))
                })
                .map(|rows| rows.filter_map(|x| x.ok()).collect())
                .ok()
            })
            .unwrap_or_default(),
    };
    let subject = body.get("subject").and_then(|v| v.as_str()).unwrap_or("");
    let body_text = body.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let rendered = crate::outreach::render_for(&conn, customer_id, &tickets, subject, body_text);
    let sources: Vec<Value> = tickets
        .iter()
        .take(3)
        .map(|t| json!({ "number": t["number"], "subject": t["subject"] }))
        .collect();
    Json(json!({
        "rendered": rendered,
        "customer": { "first_name": first_name, "last_name": last_name, "email": email },
        "sources": sources,
    }))
    .into_response()
}

/// POST /api/outreach/campaigns/:id/preview — render for one recipient.
pub async fn campaign_preview_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return campaign_not_found();
    };
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conn = state.conn_lock();
    let Some(c) = crate::outreach::get_outreach_campaign(&conn, id)
        .ok()
        .flatten()
    else {
        return campaign_not_found();
    };
    let customer_local_id = body.get("customer_local_id").and_then(|v| v.as_i64());
    let recipient: Option<(i64, Option<String>, Option<String>, String)> = customer_local_id
        .and_then(|cid| {
            conn.query_row(
                "SELECT customer_local_id, email, first_name, snapshot
                   FROM outreach_recipients
                  WHERE campaign_id = ?1 AND customer_local_id = ?2",
                rusqlite::params![id, cid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok()
        });
    // first_name is not on outreach_recipients — join customers.
    let recipient: Option<(i64, String, String, String, Option<String>)> = customer_local_id
        .and_then(|cid| {
            conn.query_row(
                "SELECT r.customer_local_id, COALESCE(r.email, ''), r.snapshot, '',
                        (c.first_name || ' ' || c.last_name)
                   FROM outreach_recipients r
                   LEFT JOIN customers c ON c.id = r.customer_local_id
                  WHERE r.campaign_id = ?1 AND r.customer_local_id = ?2",
                rusqlite::params![id, cid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .ok()
        });
    let Some((_clid, email, snapshot, _unused, name)) = recipient else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404, "error": "NotFound",
                "message": "That customer is not a recipient of this campaign."
            })),
        )
            .into_response();
    };
    let _ = _unused;
    let tickets: Vec<Value> = serde_json::from_str::<Value>(&snapshot)
        .ok()
        .and_then(|v| v.get("matching_tickets").cloned())
        .and_then(|t| serde_json::from_value(t).ok())
        .unwrap_or_default();
    let rendered = crate::outreach::render_for(&conn, _clid, &tickets, &c.subject, &c.body);
    let sources: Vec<Value> = tickets
        .iter()
        .take(3)
        .map(|t| json!({ "number": t["number"], "subject": t["subject"] }))
        .collect();
    let _ = email;
    let name = name.unwrap_or_default();
    let (first, last) = match name.split_once(' ') {
        Some((f, l)) => (f.to_string(), l.trim().to_string()),
        None => (name.clone(), String::new()),
    };
    Json(json!({
        "rendered": rendered,
        "customer": { "first_name": first, "last_name": last, "email": email },
        "sources": sources,
    }))
    .into_response()
}

fn campaign_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "statusCode": 404, "error": "NotFound",
            "message": "Campaign not found."
        })),
    )
        .into_response()
}

/// POST /api/outreach/campaigns/:id/queue
pub async fn campaign_queue_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return Json(json!({ "ok": false, "message": "Campaign not found." })).into_response();
    };
    let conn = state.conn_lock();
    let result = crate::outreach::campaign_queue(&conn, id);
    if result["ok"].as_bool().unwrap_or(false) {
        let _ = crate::jobs::audit(
            &conn,
            "user",
            "campaign_queued",
            None,
            None,
            Some(&json!({ "id": id }).to_string()),
            None,
            None,
            false,
        );
    }
    Json(result).into_response()
}

/// POST /api/outreach/campaigns/:id/pause
pub async fn campaign_pause_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return Json(json!({ "ok": false, "message": "Campaign not found." })).into_response();
    };
    let conn = state.conn_lock();
    Json(crate::outreach::campaign_pause(&conn, id)).into_response()
}

/// POST /api/outreach/campaigns/:id/resume
pub async fn campaign_resume_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return Json(json!({ "ok": false, "message": "Campaign not found." })).into_response();
    };
    let conn = state.conn_lock();
    Json(crate::outreach::campaign_resume(&conn, id)).into_response()
}

/// POST /api/outreach/campaigns/:id/cancel
pub async fn campaign_cancel_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return Json(json!({ "ok": false, "message": "Campaign not found." })).into_response();
    };
    let conn = state.conn_lock();
    Json(crate::outreach::campaign_cancel_remaining(&conn, id)).into_response()
}

/// POST /api/outreach/campaigns/:id/retry
pub async fn campaign_retry_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return Json(json!({ "ok": false, "message": "Campaign not found." })).into_response();
    };
    let conn = state.conn_lock();
    Json(crate::outreach::campaign_retry_failed(&conn, id)).into_response()
}

/// POST /api/outreach/campaigns/:id/reconcile
pub async fn campaign_reconcile_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return Json(json!({ "ok": true,
            "message": "Reconciliation finished: 0 resolved as sent, 0 returned to the queue, 0 still unknown.",
            "resolvedSent": 0, "returnedToQueue": 0, "stillUnknown": 0 })).into_response();
    };
    let conn = state.conn_lock();
    let r = crate::outreach::campaign_reconcile(&conn, id);
    let resolved = r["resolvedSent"].as_i64().unwrap_or(0);
    let returned = r["returnedToQueue"].as_i64().unwrap_or(0);
    let unknown = r["stillUnknown"].as_i64().unwrap_or(0);
    Json(json!({
        "ok": true,
        "message": format!("Reconciliation finished: {resolved} resolved as sent, {returned} returned to the queue, {unknown} still unknown."),
        "resolvedSent": resolved, "returnedToQueue": returned, "stillUnknown": unknown,
    }))
    .into_response()
}

/// GET /api/outreach/campaigns/:id/report
pub async fn campaign_report_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return Json(crate::outreach::campaign_report_not_found()).into_response();
    };
    let conn = state.conn_lock();
    Json(crate::outreach::campaign_report(&conn, id)).into_response()
}

/// DELETE /api/outreach/campaigns/:id
pub async fn campaign_delete_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return Json(json!({ "ok": false, "message": "Campaign not found." })).into_response();
    };
    let conn = state.conn_lock();
    Json(crate::outreach::campaign_delete(&conn, id)).into_response()
}

/// GET /api/outreach/dnc
pub async fn list_dnc_route(State(state): State<AppState>) -> Response {
    let conn = state.conn_lock();
    // Reference listDnc: customer_local_id, first_name, last_name, reason,
    // created_at (joined from customers).
    let dnc: Vec<Value> = conn
        .prepare(
            "SELECT d.customer_id, c.first_name, c.last_name, d.reason, d.created_at
               FROM do_not_contact d
               LEFT JOIN customers c ON c.id = d.customer_id
              ORDER BY d.created_at DESC",
        )
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "customer_local_id": r.get::<_, i64>(0)?,
                    "first_name": r.get::<_, Option<String>>(1)?,
                    "last_name": r.get::<_, Option<String>>(2)?,
                    "reason": r.get::<_, Option<String>>(3)?,
                    "created_at": r.get::<_, String>(4)?,
                }))
            })
            .map(|rows| rows.filter_map(|x| x.ok()).collect())
            .ok()
        })
        .unwrap_or_default();
    Json(json!({ "dnc": dnc })).into_response()
}

/// POST /api/outreach/dnc
pub async fn add_dnc_route(State(state): State<AppState>, body: Option<Json<Value>>) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let id = body
        .get("customer_local_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if id <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422, "error": "ValidationError",
                "message": "customer_local_id must be a positive integer."
            })),
        )
            .into_response();
    }
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .map(String::from);
    let conn = state.conn_lock();
    let _ = conn.execute(
        "INSERT INTO do_not_contact (customer_id, reason, created_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(customer_id) DO UPDATE SET reason = excluded.reason",
        rusqlite::params![id, reason],
    );
    let _ = crate::jobs::audit(
        &conn,
        "user",
        "dnc_added",
        None,
        None,
        Some(&json!({ "customer_local_id": id }).to_string()),
        None,
        None,
        false,
    );
    Json(json!({
        "ok": true,
        "message": "Added to Do-Not-Contact. Every future campaign skips this customer."
    }))
    .into_response()
}

/// DELETE /api/outreach/dnc/:customerLocalId
pub async fn remove_dnc_route(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422, "error": "ValidationError",
                "message": "A positive numeric customer id is required."
            })),
        )
            .into_response();
    };
    if id <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422, "error": "ValidationError",
                "message": "A positive numeric customer id is required."
            })),
        )
            .into_response();
    }
    let conn = state.conn_lock();
    let removed = conn
        .execute("DELETE FROM do_not_contact WHERE customer_id = ?1", [id])
        .map(|n| n > 0)
        .unwrap_or(false);
    if !removed {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404, "error": "NotFound",
                "message": "Customer is not on the Do-Not-Contact list."
            })),
        )
            .into_response();
    }
    Json(json!({ "ok": true, "message": "Removed from Do-Not-Contact." })).into_response()
}
