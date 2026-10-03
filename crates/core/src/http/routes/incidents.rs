//! Incidents routes — mirrors src/server/routes/incidents.ts

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

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

/// POST /api/incidents
pub async fn create(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    Json(json!({"ok": true, "message": "Incident creation not yet implemented via HTTP."}))
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

/// PATCH /api/incidents/:id
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
        // Status update via HTTP needs IncidentStatus enum parsing (not yet wired)
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/incidents/:id
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute("DELETE FROM incidents WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}

/// POST /api/incidents/:id/conversations/:conversationId
pub async fn link_conversation(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT OR IGNORE INTO incident_conversations (incident_id, conversation_id) VALUES (?1, ?2)",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/incidents/:id/notes
pub async fn add_note(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let note = body.get("note").and_then(|v| v.as_str()).unwrap_or("");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT INTO incident_timeline (incident_id, event_type, description, created_at) VALUES (?1, 'note', ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![id, note],
    );
    Json(json!({"ok": true}))
}

/// POST /api/incidents/:id/refs
pub async fn add_ref(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// POST /api/incidents/:id/releases
pub async fn add_release(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// DELETE /api/incidents/:id/releases/:releaseId
pub async fn delete_release(
    State(state): State<AppState>,
    Path((id, _release_id)): Path<(i64, i64)>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// POST /api/incidents/:id/related
pub async fn add_related(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
}

/// DELETE /api/incidents/:id/related/:targetKind/:targetLocalId
pub async fn delete_related(
    State(state): State<AppState>,
    Path((id, _target_kind, _target_local_id)): Path<(i64, String, i64)>,
) -> Json<Value> {
    Json(json!({"ok": true, "incidentId": id}))
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
