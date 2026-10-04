//! Incidents routes — mirrors src/server/routes/incidents.ts

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// The reference `IncidentService.notify` — one `incident_update`
/// notification through the Notification Center funnel. The dedup key
/// includes the minute (re-notifying the same transition twice in one
/// minute dedups; a later repeat is a genuinely new event).
fn notify_incident_update(
    conn: &rusqlite::Connection,
    bus: Option<&crate::http::EventBus>,
    incident_id: i64,
    title: &str,
    body: &str,
    severity: &str,
    actor_user_id: Option<i64>,
) {
    let Some(incident) = crate::intelligence_features::incident_row_json(conn, incident_id) else {
        return;
    };
    let code = incident
        .get("code")
        .and_then(|c| c.as_str())
        .unwrap_or("INC");
    let minute = chrono::Utc::now().format("%Y-%m-%dT%H:%M").to_string();
    let _ = crate::notifications::record_notification(
        conn,
        bus,
        &crate::notifications::NotificationInput {
            notification_type: spp_catalog::NotificationType::IncidentUpdate,
            severity: Some(severity),
            title: format!("{code}: {title}"),
            body: Some(body.to_string()),
            actor_user_local_id: actor_user_id,
            dedup_key: format!("incident_update:{incident_id}:{title}:{minute}"),
            ..Default::default()
        },
    );
}

/// GET /api/incidents
pub async fn list(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let incidents = crate::intelligence_features::list_incidents(&conn, None).unwrap_or_default();
    let items: Vec<Value> = incidents
        .iter()
        .filter_map(|i| serde_json::to_value(i).ok())
        .collect();
    let total = items.len() as i64;
    Json(json!({"incidents": items, "total": total}))
}

/// POST /api/incidents — create a manual incident (reference incidents.ts:31).
///
/// incidentCreateSchema: title (1..200 after trim), status/severity enums
/// with defaults investigating/sev3, nullable bounded text fields, and up to
/// 500 conversation ids. Violations are 400 BadRequest with the joined
/// issue messages (reference zod formatting, capped at 300 chars).
pub async fn create(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let mut issues: Vec<String> = Vec::new();

    let title = match body.get("title") {
        Some(Value::String(t)) => {
            let trimmed = t.trim();
            if trimmed.is_empty() {
                issues.push("Title is required.".to_string());
                None
            } else if trimmed.chars().count() > 200 {
                issues.push("Title must be at most 200 characters.".to_string());
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => {
            issues.push("Title is required.".to_string());
            None
        }
    };
    let status = match body.get("status") {
        None | Some(Value::Null) => Some("investigating".to_string()),
        Some(Value::String(s)) => {
            if INCIDENT_STATUSES.contains(&s.as_str()) {
                Some(s.clone())
            } else {
                issues.push(format!(
                    "Invalid enum value. Expected 'investigating' | 'identified' | 'fix_in_progress' | 'monitoring' | 'resolved', received '{s}'."
                ));
                None
            }
        }
        Some(_) => {
            issues.push("Expected string, received non-string.".to_string());
            None
        }
    };
    let severity = match body.get("severity") {
        None | Some(Value::Null) => Some("sev3".to_string()),
        Some(Value::String(s)) => {
            if INCIDENT_SEVERITIES.contains(&s.as_str()) {
                Some(s.clone())
            } else {
                issues.push(format!(
                    "Invalid enum value. Expected 'sev1' | 'sev2' | 'sev3' | 'sev4', received '{s}'."
                ));
                None
            }
        }
        Some(_) => {
            issues.push("Expected string, received non-string.".to_string());
            None
        }
    };

    fn bounded_text(
        body: &Value,
        key: &str,
        max: usize,
        issues: &mut Vec<String>,
    ) -> Option<Option<String>> {
        match body.get(key) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(s)) => {
                let trimmed = s.trim();
                if trimmed.chars().count() > max {
                    issues.push(format!("{key} must be at most {max} characters."));
                    None
                } else {
                    Some(Some(trimmed.to_string()))
                }
            }
            Some(_) => {
                issues.push(format!("Expected string, received non-string for {key}."));
                None
            }
        }
    }
    let owner_user_local_id = match body.get("ownerUserId") {
        None | Some(Value::Null) => Some(None),
        Some(Value::Number(n)) if n.as_i64().is_some_and(|v| v > 0) => Some(n.as_i64()),
        Some(_) => {
            issues.push("ownerUserId must be a positive integer or null.".to_string());
            None
        }
    };
    let product = bounded_text(&body, "product", 120, &mut issues);
    let feature = bounded_text(&body, "feature", 120, &mut issues);
    let description = bounded_text(&body, "description", 4000, &mut issues);
    let internal_explanation = bounded_text(&body, "internalExplanation", 8000, &mut issues);
    let customer_safe_explanation =
        bounded_text(&body, "customerSafeExplanation", 8000, &mut issues);
    let known_cause = bounded_text(&body, "knownCause", 4000, &mut issues);
    let workaround = bounded_text(&body, "workaround", 4000, &mut issues);
    let resolution = bounded_text(&body, "resolution", 4000, &mut issues);
    let started_at = match body.get("startedAt") {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(s)) => {
            // isoDateish: a parseable date string or null.
            if chrono::DateTime::parse_from_rfc3339(s).is_ok() {
                Some(Some(s.clone()))
            } else {
                issues.push("startedAt must be an ISO 8601 date string or null.".to_string());
                None
            }
        }
        Some(_) => {
            issues.push("startedAt must be a string or null.".to_string());
            None
        }
    };
    let conversation_ids: Option<Vec<i64>> = match body.get("conversationIds") {
        None => Some(Vec::new()),
        Some(Value::Array(arr)) => {
            if arr.len() > 500 {
                issues.push("conversationIds must contain at most 500 items.".to_string());
                None
            } else {
                let mut ids = Vec::with_capacity(arr.len());
                let mut ok = true;
                for v in arr {
                    match v.as_i64() {
                        Some(id) if id > 0 => ids.push(id),
                        _ => {
                            issues.push(
                                "conversationIds must contain positive integers.".to_string(),
                            );
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    Some(ids)
                } else {
                    None
                }
            }
        }
        Some(_) => {
            issues.push("conversationIds must be an array.".to_string());
            None
        }
    };

    if !issues.is_empty() {
        let message: String = issues.join("; ");
        let message = message.chars().take(300).collect::<String>();
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"statusCode": 400, "error": "BadRequest", "message": message})),
        )
            .into_response();
    }

    let (
        title,
        status,
        severity,
        owner_user_local_id,
        product,
        feature,
        description,
        internal_explanation,
        customer_safe_explanation,
        known_cause,
        workaround,
        resolution,
        started_at,
        conversation_ids,
    ) = (
        title.unwrap(),
        status.unwrap(),
        severity.unwrap(),
        owner_user_local_id.flatten(),
        product.flatten(),
        feature.flatten(),
        description.flatten(),
        internal_explanation.flatten(),
        customer_safe_explanation.flatten(),
        known_cause.flatten(),
        workaround.flatten(),
        resolution.flatten(),
        started_at.flatten(),
        conversation_ids.unwrap_or_default(),
    );

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Conversation ids are validated against the mirror before insert
    // (reference: only existing, non-deleted conversations are linked).
    let mut valid_ids = Vec::with_capacity(conversation_ids.len());
    for id in &conversation_ids {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT id FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .ok();
        if exists.is_some() {
            valid_ids.push(*id);
        }
    }

    let incident = crate::intelligence_features::ManualIncident {
        title,
        status,
        severity,
        owner_user_local_id,
        product,
        feature,
        description,
        internal_explanation,
        customer_safe_explanation,
        known_cause,
        workaround,
        resolution,
        started_at,
        conversation_ids: valid_ids,
    };
    match crate::intelligence_features::create_manual_incident(&conn, &incident) {
        Ok(id) => {
            let incident_json =
                crate::intelligence_features::incident_row_json(&conn, id).unwrap_or(json!({}));
            let _ = crate::audit::audit(
                &conn,
                &crate::audit::AuditEntry::user("incident_created").with_after_state(json!({
                    "id": id,
                    "code": incident_json.get("code"),
                    "title": incident_json.get("title"),
                })),
            );
            // Reference IncidentService.create: fan out one incident_update
            // ("incident declared") with the severity/status summary.
            let linked = conversation_ids.len();
            let body = if linked > 0 {
                format!(
                    "Severity {}, status {}, {linked} linked conversation(s).",
                    incident.severity, incident.status
                )
            } else {
                format!(
                    "Severity {}, status {}.",
                    incident.severity, incident.status
                )
            };
            notify_incident_update(
                &conn,
                Some(&state.bus),
                id,
                "incident declared",
                &body,
                "warning",
                owner_user_local_id,
            );
            (
                StatusCode::OK,
                Json(json!({"ok": true, "incident": incident_json})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"ok": false, "message": format!("Could not create incident: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/incidents/:id
pub async fn get(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let incidents = crate::intelligence_features::list_incidents(&conn, None).unwrap_or_default();
    if let Some(incident) = incidents.iter().find(|i| i.id == Some(id)) {
        Json(serde_json::to_value(incident).unwrap_or(json!({})))
    } else {
        Json(json!({"error": "Incident not found"}))
    }
}

/// PATCH /api/incidents/:id — the reference `IncidentService.patch`
/// notification-relevant subset: status and severity transitions (with
/// `incident_update` fan-out + timeline events). Unknown ids 404; unknown
/// enum values are ignored (the reference zod-parses the whole body and
/// rejects, but the port's stub accepted everything — the field-level
/// subset keeps the route honest without widening scope).
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let Some(before) = crate::intelligence_features::incident_row_json(&conn, id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Incident not found."
            })),
        )
            .into_response();
    };
    let actor_user_id = body.get("actorUserId").and_then(|v| v.as_i64());
    let status = body
        .get("status")
        .and_then(|v| v.as_str())
        .filter(|s| INCIDENT_STATUSES.contains(s));
    let severity = body
        .get("severity")
        .and_then(|v| v.as_str())
        .filter(|s| INCIDENT_SEVERITIES.contains(s));

    let mut updated = false;
    if let Some(status) = status {
        let before_status = before.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status != before_status {
            conn.execute(
                "UPDATE incidents SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
                rusqlite::params![status, id],
            )
            .map(|_| ())
            .ok();
            let _ = crate::intelligence_features::record_incident_event(
                &conn,
                id,
                "status_changed",
                actor_user_id,
                Some(&json!({
                    "from": before_status,
                    "to": status,
                })),
                &format!("incident:{id}:status:{before_status}:{status}"),
            );
            let body = if status == "resolved" {
                "Incident resolved.".to_string()
            } else {
                format!("Status moved from {before_status} to {status}.")
            };
            let severity = if status == "resolved" {
                "info"
            } else {
                "warning"
            };
            notify_incident_update(
                &conn,
                Some(&state.bus),
                id,
                &format!("status changed to {status}"),
                &body,
                severity,
                actor_user_id,
            );
            updated = true;
        }
    }
    if let Some(severity) = severity {
        let before_severity = before
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if severity != before_severity {
            conn.execute(
                "UPDATE incidents SET severity = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
                rusqlite::params![severity, id],
            )
            .map(|_| ())
            .ok();
            let _ = crate::intelligence_features::record_incident_event(
                &conn,
                id,
                "severity_changed",
                actor_user_id,
                Some(&json!({
                    "from": before_severity,
                    "to": severity,
                })),
                &format!("incident:{id}:severity:{before_severity}:{severity}"),
            );
            notify_incident_update(
                &conn,
                Some(&state.bus),
                id,
                &format!("severity changed to {severity}"),
                &format!("Severity moved from {before_severity} to {severity}."),
                "critical",
                actor_user_id,
            );
            updated = true;
        }
    }
    if !updated {
        return (
            StatusCode::OK,
            Json(json!({"ok": true, "incident": before})),
        )
            .into_response();
    }
    let incident =
        crate::intelligence_features::incident_row_json(&conn, id).unwrap_or(before.clone());
    (
        StatusCode::OK,
        Json(json!({"ok": true, "incident": incident})),
    )
        .into_response()
}

/// DELETE /api/incidents/:id
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute("DELETE FROM incidents WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}

/// POST /api/incidents/:id/conversations/:conversationId — the reference
/// `IncidentService.linkConversation`: link + `incident_update` fan-out
/// when a NEW link was created.
pub async fn link_conversation(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let actor_user_id: Option<i64> = None;
    let created = crate::intelligence_features::link_incident_conversation(
        &conn,
        id,
        conversation_id,
        "human",
    )
    .unwrap_or(false);
    if created {
        let number: Option<i64> = conn
            .query_row(
                "SELECT number FROM conversations WHERE id = ?1 OR remote_id = ?1",
                rusqlite::params![conversation_id],
                |r| r.get(0),
            )
            .ok();
        let body = number.map_or_else(
            || "A conversation was linked to this incident.".to_string(),
            |n| format!("Conversation #{n} is now counted in this incident."),
        );
        notify_incident_update(
            &conn,
            Some(&state.bus),
            id,
            "conversation linked",
            &body,
            "info",
            actor_user_id,
        );
    }
    Json(json!({"ok": true, "created": created}))
}

/// POST /api/incidents/:id/notes — the reference `IncidentService.addNote`
/// (body: `{ body }`, 1..4000 after trim; the reference UI sends `body`).
pub async fn add_note(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let note = body
        .get("body")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if note.is_empty() {
        return Json(json!({
            "ok": false,
            "message": "Note body is required."
        }));
    }
    if note.chars().count() > 4000 {
        return Json(json!({
            "ok": false,
            "message": "Note body must be at most 4000 characters."
        }));
    }
    let author = body.get("authorUserId").and_then(|v| v.as_i64());
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::add_incident_note(&conn, id, author, note) {
        Ok(_) => Json(json!({ "ok": true })),
        Err(e) => Json(json!({ "ok": false, "message": e.to_string() })),
    }
}

/// POST /api/incidents/:id/refs — the reference addRef (body:
/// `{ system, reference }`; the reference UI prompts for both).
pub async fn add_ref(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let system = body.get("system").and_then(|v| v.as_str()).map(str::trim);
    let reference = body
        .get("reference")
        .and_then(|v| v.as_str())
        .map(str::trim);
    let (Some(system), Some(reference)) = (system, reference) else {
        return Json(json!({
            "ok": false,
            "message": "Both a reference system and a reference id are required."
        }));
    };
    if system.is_empty()
        || system.chars().count() > 50
        || reference.is_empty()
        || reference.chars().count() > 200
    {
        return Json(json!({
            "ok": false,
            "message": "Invalid reference system or id."
        }));
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::add_incident_ref(&conn, id, system, reference) {
        Ok(_) => Json(json!({ "ok": true, "incidentId": id })),
        Err(e) => Json(json!({ "ok": false, "message": e.to_string() })),
    }
}

/// POST /api/incidents/:id/releases — the reference addRelease (body:
/// `{ versionLabel, releasedAt?, notes? }`; the UI modal sends all three).
pub async fn add_release(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let label = body
        .get("versionLabel")
        .and_then(|v| v.as_str())
        .map(str::trim);
    let Some(label) = label.filter(|l| !l.is_empty()) else {
        return Json(json!({
            "ok": false,
            "message": "A version label is required."
        }));
    };
    if label.chars().count() > 100 {
        return Json(json!({
            "ok": false,
            "message": "Version label must be at most 100 characters."
        }));
    }
    let released_at = body
        .get("releasedAt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let notes = body
        .get("notes")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::add_incident_release(&conn, id, label, released_at, notes) {
        Ok(0) => Json(json!({
            "ok": false,
            "message": "That version label is already attached to this incident."
        })),
        Ok(_) => Json(json!({ "ok": true, "incidentId": id })),
        Err(e) => Json(json!({ "ok": false, "message": e.to_string() })),
    }
}

/// DELETE /api/incidents/:id/releases/:releaseId
pub async fn delete_release(
    State(state): State<AppState>,
    Path((id, release_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::delete_incident_release(&conn, id, release_id) {
        Ok(true) => Json(json!({ "ok": true, "incidentId": id })),
        Ok(false) => Json(json!({
            "ok": false,
            "message": "Release not found on this incident."
        })),
        Err(e) => Json(json!({ "ok": false, "message": e.to_string() })),
    }
}

/// POST /api/incidents/:id/related — the reference addRelated (body:
/// `{ targetKind, targetLocalId, note? }`).
pub async fn add_related(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let Some(kind) = body.get("targetKind").and_then(|v| v.as_str()) else {
        return Json(json!({
            "ok": false,
            "message": "targetKind is required."
        }));
    };
    if !matches!(
        kind,
        "known_issue" | "knowledge_doc" | "campaign" | "custom_object"
    ) {
        return Json(json!({
            "ok": false,
            "message": "Invalid targetKind."
        }));
    }
    let Some(target_local_id) = body.get("targetLocalId").and_then(|v| v.as_i64()) else {
        return Json(json!({
            "ok": false,
            "message": "targetLocalId is required."
        }));
    };
    let note = body
        .get("note")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::add_incident_related(&conn, id, kind, target_local_id, note)
    {
        Ok(_) => Json(json!({ "ok": true, "incidentId": id })),
        Err(e) => Json(json!({ "ok": false, "message": e.to_string() })),
    }
}

/// DELETE /api/incidents/:id/related/:targetKind/:targetLocalId
pub async fn delete_related(
    State(state): State<AppState>,
    Path((id, target_kind, target_local_id)): Path<(i64, String, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::delete_incident_related(
        &conn,
        id,
        &target_kind,
        target_local_id,
    ) {
        Ok(true) => Json(json!({ "ok": true, "incidentId": id })),
        Ok(false) => Json(json!({
            "ok": false,
            "message": "That related entity is not linked to this incident."
        })),
        Err(e) => Json(json!({ "ok": false, "message": e.to_string() })),
    }
}

/// GET /api/incidents/:id/impact
pub async fn impact(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let linked: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM incident_conversations WHERE incident_id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({"impact": {"linked_conversations": linked, "severity": "unknown"}}))
}

// ─── Declare-an-incident routes (reference incidents.ts:289-351) ─────────

/// The overrides the from-cluster / from-known-issue bodies accept
/// (incidentCreateSchema). An INVALID body is silently ignored — the
/// reference safeParses and falls back to the derived defaults
/// (`overrides = body.success ? body.data : null`).
struct IncidentOverrides {
    title: Option<String>,
    severity: Option<String>,
    status: Option<String>,
    description: Option<String>,
}

const INCIDENT_STATUSES: [&str; 5] = [
    "investigating",
    "identified",
    "fix_in_progress",
    "monitoring",
    "resolved",
];
const INCIDENT_SEVERITIES: [&str; 4] = ["sev1", "sev2", "sev3", "sev4"];

/// incidentCreateSchema.safeParse reduced to the fields the port persists:
/// title (trimmed 1..200), status/severity (closed enums), description
/// (≤ 4000). Any violation yields `None` (defaults apply), mirroring the
/// reference's `overrides = null` path.
fn parse_incident_overrides(body: Option<Json<Value>>) -> Option<IncidentOverrides> {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let title = match body.get("title") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => {
            let t = t.trim();
            if t.is_empty() || t.chars().count() > 200 {
                return None;
            }
            Some(t.to_string())
        }
        Some(_) => return None,
    };
    let status = match body.get("status") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if !INCIDENT_STATUSES.contains(&s.as_str()) {
                return None;
            }
            Some(s.clone())
        }
        Some(_) => return None,
    };
    let severity = match body.get("severity") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if !INCIDENT_SEVERITIES.contains(&s.as_str()) {
                return None;
            }
            Some(s.clone())
        }
        Some(_) => return None,
    };
    let description = match body.get("description") {
        None | Some(Value::Null) => None,
        Some(Value::String(d)) => {
            if d.chars().count() > 4000 {
                return None;
            }
            Some(d.clone())
        }
        Some(_) => return None,
    };
    Some(IncidentOverrides {
        title,
        severity,
        status,
        description,
    })
}

/// POST /api/incidents/from-cluster/:clusterId — declare an incident FROM an
/// existing issue cluster: pre-fills the workspace and links every member
/// conversation in one action (reference incidents.ts:289-317).
pub async fn from_cluster(
    State(state): State<AppState>,
    Path(cluster_id): Path<i64>,
    body: Option<Json<Value>>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // getCluster: the cluster row + its member conversation ids.
    let cluster: Option<(String, Option<i64>)> = conn
        .query_row(
            "SELECT name, known_issue_id FROM issue_clusters WHERE id = ?1",
            rusqlite::params![cluster_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((cluster_name, cluster_known_issue_id)) = cluster else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Issue cluster not found."
            })),
        )
            .into_response();
    };
    let conversation_ids =
        crate::intelligence_features::cluster_conversation_ids(&conn, cluster_id);
    let overrides = parse_incident_overrides(body);
    let title = overrides
        .as_ref()
        .and_then(|o| o.title.clone())
        .unwrap_or_else(|| format!("Issue cluster: {cluster_name}"));
    let severity = overrides
        .as_ref()
        .and_then(|o| o.severity.clone())
        .unwrap_or_else(|| "sev3".into());
    let status = overrides
        .as_ref()
        .and_then(|o| o.status.clone())
        .unwrap_or_else(|| "investigating".into());
    let description = overrides
        .as_ref()
        .and_then(|o| o.description.clone())
        .unwrap_or_else(|| {
            // The reference appends the cluster summary when present; the port's
            // clusters carry only a name.
            format!("Declared from issue cluster \"{cluster_name}\".")
        });
    let incident_id = match crate::intelligence_features::create_incident_from_source(
        &conn,
        cluster_known_issue_id,
        &severity,
        &status,
        "cluster",
        &title,
        Some(&description),
        &conversation_ids,
    ) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"ok": false, "message": format!("Could not create incident: {e}")})),
            )
                .into_response();
        }
    };
    let incident = crate::intelligence_features::incident_row_json(&conn, incident_id)
        .unwrap_or_else(|| json!({}));
    let _ = crate::audit::audit(
        &conn,
        &crate::audit::AuditEntry::user("incident_created_from_cluster").with_after_state(json!({
            "id": incident_id,
            "code": incident.get("code"),
            "cluster_id": cluster_id,
        })),
    );
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "incident": incident,
            "linked_conversations": conversation_ids.len(),
        })),
    )
        .into_response()
}

/// POST /api/incidents/from-known-issue/:knownIssueId — declare an incident
/// FROM a known issue: carries the explanations over and links the known
/// issue's conversations (reference incidents.ts:321-351).
pub async fn from_known_issue(
    State(state): State<AppState>,
    Path(known_issue_id): Path<i64>,
    body: Option<Json<Value>>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // getKnownIssue: the issue row + its linked conversation ids (the port
    // stores them in known_issue_links, M015's known_issue_conversations).
    let known_issue: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT name, description FROM known_issues WHERE id = ?1",
            rusqlite::params![known_issue_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((issue_name, issue_description)) = known_issue else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Known issue not found."
            })),
        )
            .into_response();
    };
    let conversation_ids =
        crate::intelligence_features::known_issue_conversation_ids(&conn, known_issue_id);
    let overrides = parse_incident_overrides(body);
    let title = overrides
        .as_ref()
        .and_then(|o| o.title.clone())
        .unwrap_or_else(|| format!("Known issue: {issue_name}"));
    let severity = overrides
        .as_ref()
        .and_then(|o| o.severity.clone())
        .unwrap_or_else(|| "sev3".into());
    let status = overrides
        .as_ref()
        .and_then(|o| o.status.clone())
        .unwrap_or_else(|| "investigating".into());
    let description = overrides
        .as_ref()
        .and_then(|o| o.description.clone())
        .unwrap_or_else(|| {
            // The reference appends the issue's symptoms; the port's
            // known_issues carry a free-text description in that role.
            match issue_description.as_deref().map(str::trim) {
                Some(d) if !d.is_empty() => {
                    format!("Declared from known issue \"{issue_name}\". {d}")
                }
                _ => format!("Declared from known issue \"{issue_name}\"."),
            }
        });
    let incident_id = match crate::intelligence_features::create_incident_from_source(
        &conn,
        Some(known_issue_id),
        &severity,
        &status,
        "known_issue",
        &title,
        Some(&description),
        &conversation_ids,
    ) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"ok": false, "message": format!("Could not create incident: {e}")})),
            )
                .into_response();
        }
    };
    // The reference also records the known-issue relation on the incident.
    let _ = crate::intelligence_features::add_incident_related(
        &conn,
        incident_id,
        "known_issue",
        known_issue_id,
        Some("Declared from this known issue"),
    );
    let incident = crate::intelligence_features::incident_row_json(&conn, incident_id)
        .unwrap_or_else(|| json!({}));
    let _ = crate::audit::audit(
        &conn,
        &crate::audit::AuditEntry::user("incident_created_from_known_issue").with_after_state(
            json!({
                "id": incident_id,
                "code": incident.get("code"),
                "known_issue_id": known_issue_id,
            }),
        ),
    );
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "incident": incident,
            "linked_conversations": conversation_ids.len(),
        })),
    )
        .into_response()
}

/// DELETE /api/incidents/:id/conversations/:conversationId — unlink a
/// conversation from an incident (reference incidents.ts:154-163).
pub async fn unlink_conversation(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM incidents WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Incident not found."
            })),
        )
            .into_response();
    }
    let removed =
        crate::intelligence_features::unlink_incident_conversation(&conn, id, conversation_id)
            .unwrap_or(false);
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "removed": removed,
            "message": if removed {
                "Conversation unlinked."
            } else {
                "Conversation was not linked."
            },
        })),
    )
        .into_response()
}

/// DELETE /api/incidents/:id/refs/:refId — remove an engineering reference
/// from an incident (reference incidents.ts:195-204).
pub async fn delete_ref(
    State(state): State<AppState>,
    Path((id, ref_id)): Path<(i64, i64)>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let removed =
        crate::intelligence_features::delete_incident_ref(&conn, id, ref_id).unwrap_or(false);
    if !removed {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Reference not found on this incident."
            })),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        Json(json!({"ok": true, "message": "Reference removed."})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use rusqlite::{params, Connection};
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
        crate::intelligence::apply_m014(&conn).unwrap();
        crate::intelligence_features::apply_m015_to_m019(&conn).unwrap();
        crate::intelligence_features::apply_m035(&conn).unwrap();
        conn
    }

    fn make_state() -> AppState {
        let conn = fresh_db();
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
            qdrant: std::sync::Arc::new(crate::vectorstore_qdrant::EmbeddedQdrant::new(
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
    async fn create_requires_title() {
        let state = make_state();
        let (status, body) = body_json(
            create(
                State(state),
                Json(json!({"status": "investigating", "severity": "sev3"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "BadRequest");
        assert!(body["message"].as_str().unwrap().contains("Title"));
    }

    #[tokio::test]
    async fn create_rejects_bad_status_enum() {
        let state = make_state();
        let (status, body) = body_json(
            create(
                State(state),
                Json(json!({"title": "Down", "status": "explode"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("Invalid enum value"));
    }

    #[tokio::test]
    async fn create_inserts_with_defaults_and_links_valid_conversations() {
        let state = make_state();
        {
            let conn = state.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO conversations (remote_id, number, mailbox_id, customer_id)
                 VALUES (1, 1, 1, 1)",
                [],
            )
            .unwrap();
        }
        let (status, body) = body_json(
            create(
                State(state),
                Json(json!({
                    "title": "  Checkout down  ",
                    "conversationIds": [1, 999],
                    "product": " Billing ",
                    "startedAt": "2026-10-01T00:00:00Z"
                })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        let incident = &body["incident"];
        assert_eq!(incident["title"], "Checkout down"); // trimmed
        assert_eq!(incident["status"], "investigating"); // default
        assert_eq!(incident["severity"], "sev3"); // default
        assert!(incident["code"].as_str().unwrap().starts_with("INC-"));
    }

    #[tokio::test]
    async fn create_rejects_non_string_title() {
        let state = make_state();
        let (status, _) = body_json(create(State(state), Json(json!({"title": 42}))).await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
