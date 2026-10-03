//! Issues routes — mirrors src/server/routes/issues.ts

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/issues/clusters
pub async fn list_clusters(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let clusters: Vec<Value> = conn
        .prepare("SELECT id, name, status, created_at FROM issue_clusters ORDER BY id DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"clusters": clusters}))
}

/// GET /api/issues/sla-alerts
///
/// Reference response shape:
/// ```json
/// {
///   "generated_at": "...",
///   "total_breached": N, "total_at_risk": N,
///   "alerts": [...],
///   "per_mailbox": [],
///   "unconfigured_mailboxes": [],
///   "note": "..."
/// }
/// ```
pub async fn sla_alerts(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let total_breached: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE response_state = 'sla_breached'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let total_at_risk: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE response_state = 'sla_at_risk'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // Alerts: a simple per-state rollup.
    let alerts: Vec<Value> = vec![json!({
        "state": "breached",
        "count": total_breached,
    })];
    // Per-mailbox: count breached conversations grouped by mailbox_id.
    let per_mailbox: Vec<Value> = conn
        .prepare("SELECT mailbox_id, COUNT(*) FROM conversations WHERE response_state = 'sla_breached' GROUP BY mailbox_id ORDER BY 2 DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "mailbox_id": r.get::<_, i64>(0)?,
                    "breached_count": r.get::<_, i64>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    // Unconfigured mailboxes: those without SLA configs.
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
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "total_breached": total_breached,
        "total_at_risk": total_at_risk,
        "alerts": alerts,
        "per_mailbox": per_mailbox,
        "unconfigured_mailboxes": unconfigured_mailboxes,
        "note": "Alerts measure BUSINESS minutes (nights/weekends excluded). SLA configs come from the sla_configs table."
    }))
}

/// GET /api/issues/clusters/:id
pub async fn get_cluster(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT id, name, status, created_at FROM issue_clusters WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Cluster not found"})),
    }
}

/// DELETE /api/issues/clusters/:id
pub async fn delete_cluster(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM issue_clusters WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// GET /api/issues/known
pub async fn list_known(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let issues = crate::intelligence_features::list_known_issues(&conn, None).unwrap_or_default();
    let items: Vec<Value> = issues
        .iter()
        .filter_map(|i| serde_json::to_value(i).ok())
        .collect();
    Json(json!({"known_issues": items}))
}

/// POST /api/issues/known
pub async fn create_known(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT INTO known_issues (name, status, description, created_at, updated_at) VALUES (?1, 'active', ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![name, description],
    );
    Json(json!({"ok": true}))
}

/// GET /api/issues/known/:id
pub async fn get_known(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::list_known_issues(&conn, Some("active")) {
        Ok(issues) => {
            if let Some(issue) = issues.iter().find(|i| i.id == Some(id)) {
                Json(serde_json::to_value(issue).unwrap_or(json!({})))
            } else {
                Json(json!({"error": "Known issue not found"}))
            }
        }
        Err(_) => Json(json!({"error": "Failed to list known issues"})),
    }
}

/// PATCH /api/issues/known/:id
pub async fn update_known(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        let _ = conn.execute("UPDATE known_issues SET name = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2", rusqlite::params![name, id]);
    }
    if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
        let _ = conn.execute("UPDATE known_issues SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2", rusqlite::params![status, id]);
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/issues/known/:id
pub async fn delete_known(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM known_issues WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/issues/known/:id/link/:conversationId
pub async fn link_known(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT OR IGNORE INTO known_issue_links (known_issue_id, conversation_id, link_type) VALUES (?1, ?2, 'manual')",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// DELETE /api/issues/known/:id/link/:conversationId
pub async fn unlink_known(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM known_issue_links WHERE known_issue_id = ?1 AND conversation_id = ?2",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/issues/known/:id/refs
pub async fn add_ref(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "id": id}))
}

/// GET /api/issues/cases
pub async fn list_cases(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"cases": []}))
}

/// POST /api/issues/cases/from-conversation/:conversationId
pub async fn case_from_conversation(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    Json(json!({"ok": true, "conversationId": conversation_id}))
}

/// GET /api/issues/known/:id/impact — known-issue impact (reference
/// incidents.ts:277 — `ctx.issueImpact.forKnownIssue(id)`).
///
/// Impact = conversations / distinct customers / organizations, first + last
/// seen (remote_created_at), open/closed/waiting counts, 7-day growth vs the
/// previous 7 days, trend classification, top-10 mailbox breakdown.
pub async fn known_impact(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // 404 when the known issue itself does not exist.
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM known_issues WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        return Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": "Known issue not found."
        }));
    }
    // Base counts over linked, non-deleted conversations.
    let counts_ok = conn.query_row(
        "SELECT COUNT(*),
                COUNT(DISTINCT c.customer_local_id),
                MIN(c.remote_created_at),
                MAX(c.remote_created_at),
                SUM(CASE WHEN c.status = 'active' THEN 1 ELSE 0 END),
                SUM(CASE WHEN c.status = 'closed' THEN 1 ELSE 0 END),
                SUM(CASE WHEN c.customer_waiting_since IS NOT NULL AND c.status = 'active' THEN 1 ELSE 0 END)
         FROM conversations c
         WHERE c.deleted_at IS NULL AND c.id IN (
            SELECT l.conversation_id FROM known_issue_conversations l
            WHERE l.known_issue_id = ?1
         )",
        rusqlite::params![id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                r.get::<_, Option<i64>>(6)?.unwrap_or(0),
            ))
        },
    );
    let (conversations, customers, first_seen, last_seen, open_count, closed_count, waiting_count) =
        match counts_ok {
            Ok(v) => v,
            Err(_) => (0, 0, None, None, 0, 0, 0),
        };
    // Distinct organizations among linked conversations' customers.
    let organizations: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT cu.organization_id)
             FROM conversations c JOIN customers cu ON cu.id = c.customer_local_id
             WHERE c.deleted_at IS NULL AND cu.organization_id IS NOT NULL
               AND c.id IN (
                 SELECT l.conversation_id FROM known_issue_conversations l
                 WHERE l.known_issue_id = ?1
               )",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // Growth: started in the last 7 days vs the previous 7.
    let conv_in = "|LINK|";
    let _ = conv_in;
    let recent: i64 = linked_count_since(&conn, id, "-7 days").unwrap_or(0);
    let previous: i64 = linked_count_between(&conn, id, "-14 days", "-7 days").unwrap_or(0);
    let recent14: i64 = linked_count_since(&conn, id, "-14 days").unwrap_or(0);
    let prior14: i64 = linked_count_between(&conn, id, "-28 days", "-14 days").unwrap_or(0);
    let growth = if previous > 0 {
        json!({ "window_days": 7, "recent": recent, "previous": previous,
                "ratio": (recent as f64) / (previous as f64),
                "direction": if (recent as f64) / (previous as f64) >= 1.3 { "rising" }
                             else if (recent as f64) / (previous as f64) <= 0.7 { "falling" } else { "flat" } })
    } else if recent > 0 {
        json!({ "window_days": 7, "recent": recent, "previous": previous, "ratio": null, "direction": "rising" })
    } else {
        json!({ "window_days": 7, "recent": recent, "previous": previous, "ratio": 0.0, "direction": "unknown" })
    };
    // Trend: new / rising / falling / stable (reference impact.ts compute).
    let trend = if conversations == 0 {
        "unknown".to_string()
    } else if first_seen
        .as_deref()
        .map(|fs| {
            conn.query_row(
                "SELECT julianday(?1) >= julianday('now', '-7 days')",
                rusqlite::params![fs],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v == 1)
            .unwrap_or(false)
        })
        .unwrap_or(false)
    {
        "new".to_string()
    } else if prior14 == 0 && recent14 >= 1 {
        "rising".to_string()
    } else if prior14 > 0 && recent14 < (prior14 as f64 * 0.7) as i64 {
        "falling".to_string()
    } else {
        "stable".to_string()
    };
    // Top-10 mailbox breakdown.
    let mut inboxes = Vec::new();
    let _ = conn.prepare(
        "SELECT (SELECT m.name FROM mailboxes m WHERE m.id = c.mailbox_local_id) AS mailbox, COUNT(*) AS n
         FROM conversations c
         WHERE c.deleted_at IS NULL AND c.id IN (
            SELECT l.conversation_id FROM known_issue_conversations l WHERE l.known_issue_id = ?1
         )
         GROUP BY c.mailbox_local_id ORDER BY n DESC LIMIT 10",
    )
    .and_then(|mut stmt| {
        let rows = stmt.query_map(rusqlite::params![id], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows.flatten() {
            inboxes.push(json!({ "mailbox": row.0, "conversations": row.1 }));
        }
        Ok(())
    });
    Json(json!({
        "impact": {
            "conversations": conversations,
            "customers": customers,
            "organizations": organizations,
            "first_seen": first_seen,
            "last_seen": last_seen,
            "open_count": open_count,
            "closed_count": closed_count,
            "waiting_count": waiting_count,
            "growth_rate_7d": growth,
            "trend": trend,
            "inboxes": inboxes
        }
    }))
}

/// COUNT of linked conversations created at or after `julianday('now', since)`.
fn linked_count_since(conn: &rusqlite::Connection, id: i64, since: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM conversations c
             WHERE c.deleted_at IS NULL
               AND julianday(c.remote_created_at) >= julianday('now', '{since}')
               AND c.id IN (SELECT l.conversation_id FROM known_issue_conversations l WHERE l.known_issue_id = ?1)"
        ),
        rusqlite::params![id],
        |r| r.get(0),
    )
}

/// COUNT of linked conversations created in [from, to) relative to now.
fn linked_count_between(
    conn: &rusqlite::Connection,
    id: i64,
    from: &str,
    to: &str,
) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM conversations c
             WHERE c.deleted_at IS NULL
               AND julianday(c.remote_created_at) >= julianday('now', '{from}')
               AND julianday(c.remote_created_at) < julianday('now', '{to}')
               AND c.id IN (SELECT l.conversation_id FROM known_issue_conversations l WHERE l.known_issue_id = ?1)"
        ),
        rusqlite::params![id],
        |r| r.get(0),
    )
}
