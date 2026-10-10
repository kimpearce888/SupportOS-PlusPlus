//! Incident workspace reads — the port of the reference
//! `incidentRepo`'s read side (`src/server/database/repositories/incidentRepo.ts`).
//!
//! The list view (the triage board) and the detail payload (the full
//! workspace: explanations, impact, linked conversations, affected
//! customers/organizations, related entities, refs, releases, notes,
//! timeline). Affected counts are always DERIVED (distinct customers,
//! never ticket counts) — there is no stored copy to drift.
//!
//! Port column mapping (reference → port): `customer_local_id` and
//! `mailbox_local_id` keep the reference names after DB-03/M047,
//! `remote_created_at`
//! → `created_at`, `customer_emails` table → `customers.email` column,
//! `cu.organization_id` (FK) → `cu.organization` (TEXT; resolved to an
//! `organizations.id` by name match when one exists).

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// The closed status vocabulary (reference `INCIDENT_STATUSES`).
pub const INCIDENT_STATUSES: [&str; 5] = [
    "investigating",
    "identified",
    "fix_in_progress",
    "monitoring",
    "resolved",
];

/// The closed severity vocabulary (reference `INCIDENT_SEVERITIES`).
pub const INCIDENT_SEVERITIES: [&str; 4] = ["sev1", "sev2", "sev3", "sev4"];

/// List filters for GET /api/incidents (reference incidents.ts:16-29).
#[derive(Debug, Default)]
pub struct IncidentListFilters {
    /// Exact status (validated against the closed vocabulary upstream).
    pub status: Option<String>,
    /// Exact severity (validated upstream).
    pub severity: Option<String>,
    /// `status != 'resolved'` only.
    pub open: bool,
    /// Title/code LIKE search, already clamped to 120 chars upstream.
    pub query: Option<String>,
    /// Page size, already clamped to 1..=200 upstream.
    pub limit: i64,
    /// Row offset, already >= 0 upstream.
    pub offset: i64,
}

/// The SELECT list shared by the list rows and the detail `incident`
/// object: every incidents column plus the derived counts and the owner
/// label (reference `IncidentListRow`).
const INCIDENT_ROW_SQL: &str = "i.id, i.code, i.title, i.known_issue_id, i.status, i.severity,
       i.source, i.description, i.internal_explanation, i.customer_safe_explanation,
       i.known_cause, i.workaround, i.resolution, i.started_at, i.resolved_at,
       i.owner_user_local_id, i.product, i.feature, i.created_at, i.updated_at,
       (SELECT COUNT(*) FROM incident_conversations ic WHERE ic.incident_id = i.id) AS conversation_count,
       (SELECT COUNT(DISTINCT c.customer_local_id) FROM incident_conversations ic
          JOIN conversations c ON c.id = ic.conversation_id
          WHERE ic.incident_id = i.id AND c.customer_local_id IS NOT NULL AND c.deleted_at IS NULL) AS customer_count,
       (SELECT COUNT(DISTINCT cu.organization) FROM incident_conversations ic
          JOIN conversations c ON c.id = ic.conversation_id
          JOIN customers cu ON cu.id = c.customer_local_id
          WHERE ic.incident_id = i.id AND c.deleted_at IS NULL
            AND cu.organization IS NOT NULL AND TRIM(cu.organization) != '') AS organization_count,
       (SELECT NULLIF(TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, '')), '')
          FROM users u WHERE u.id = i.owner_user_local_id) AS owner_name";

/// Reference `IncidentRepository.list`: filtered rows ordered
/// non-resolved-first, then `updated_at` DESC, with the derived counts.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] when a statement fails.
pub fn list_incident_rows(
    conn: &Connection,
    filters: &IncidentListFilters,
) -> crate::error::Result<(Vec<Value>, i64)> {
    let mut where_parts: Vec<String> = Vec::new();
    let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(status) = &filters.status {
        bound.push(Box::new(status.clone()));
        where_parts.push(format!("i.status = ?{}", bound.len()));
    }
    if let Some(severity) = &filters.severity {
        bound.push(Box::new(severity.clone()));
        where_parts.push(format!("i.severity = ?{}", bound.len()));
    }
    if filters.open {
        where_parts.push("i.status != 'resolved'".to_string());
    }
    if let Some(q) = filters.query.as_deref().filter(|q| !q.is_empty()) {
        bound.push(Box::new(format!("%{q}%")));
        where_parts.push(format!(
            "(i.title LIKE ?{} OR i.code LIKE ?{})",
            bound.len(),
            bound.len()
        ));
    }
    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    let sql = format!(
        "SELECT {INCIDENT_ROW_SQL}
         FROM incidents i {where_sql}
         ORDER BY CASE i.status WHEN 'resolved' THEN 1 ELSE 0 END, i.updated_at DESC
         LIMIT ?{} OFFSET ?{}",
        bound.len() + 1,
        bound.len() + 2
    );
    bound.push(Box::new(filters.limit.clamp(1, 200)));
    bound.push(Box::new(filters.offset.max(0)));
    let rows = query_incident_rows(conn, &sql, &bound)?;

    let total: i64 = if where_parts.is_empty() {
        conn.query_row("SELECT COUNT(*) FROM incidents i", [], |r| r.get(0))?
    } else {
        let refs: Vec<&dyn rusqlite::ToSql> = bound[..bound.len() - 2]
            .iter()
            .map(|b| b.as_ref())
            .collect();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM incidents i {where_sql}"),
            refs.as_slice(),
            |r| r.get(0),
        )?
    };
    Ok((rows, total))
}

/// The full detail payload for GET /api/incidents/:id (reference
/// incidents.ts:70-90): `None` when the incident does not exist.
#[must_use]
pub fn incident_detail(conn: &Connection, id: i64) -> Option<Value> {
    let incident = conn
        .query_row(
            &format!("SELECT {INCIDENT_ROW_SQL} FROM incidents i WHERE i.id = ?1"),
            params![id],
            incident_row_from_db,
        )
        .ok()?;
    let impact = crate::issue_impact::for_incident(conn, id)?;
    Some(json!({
        "incident": incident,
        "impact": impact,
        "conversations": list_conversations(conn, id, 200),
        "affected_customers": affected_customers(conn, id, 100),
        "affected_organizations": affected_organizations(conn, id, 50),
        "related": list_related(conn, id),
        "refs": list_refs(conn, id),
        "releases": list_releases(conn, id),
        "notes": list_notes(conn, id, 100),
        "timeline": list_events(conn, id, 200),
    }))
}

/// Run an incident-row SELECT and map every row to JSON.
fn query_incident_rows(
    conn: &Connection,
    sql: &str,
    bound: &[Box<dyn rusqlite::ToSql>],
) -> crate::error::Result<Vec<Value>> {
    let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(refs.as_slice(), incident_row_from_db)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Map one `INCIDENT_ROW_SQL` row to the reference `IncidentListRow` JSON.
fn incident_row_from_db(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "code": r.get::<_, Option<String>>(1)?,
        "title": r.get::<_, Option<String>>(2)?,
        "known_issue_id": r.get::<_, Option<i64>>(3)?,
        "status": r.get::<_, String>(4)?,
        "severity": r.get::<_, String>(5)?,
        "source": r.get::<_, String>(6)?,
        "description": r.get::<_, Option<String>>(7)?,
        "internal_explanation": r.get::<_, Option<String>>(8)?,
        "customer_safe_explanation": r.get::<_, Option<String>>(9)?,
        "known_cause": r.get::<_, Option<String>>(10)?,
        "workaround": r.get::<_, Option<String>>(11)?,
        "resolution": r.get::<_, Option<String>>(12)?,
        "started_at": r.get::<_, Option<String>>(13)?,
        "resolved_at": r.get::<_, Option<String>>(14)?,
        "owner_user_local_id": r.get::<_, Option<i64>>(15)?,
        "product": r.get::<_, Option<String>>(16)?,
        "feature": r.get::<_, Option<String>>(17)?,
        "created_at": r.get::<_, String>(18)?,
        "updated_at": r.get::<_, String>(19)?,
        "conversation_count": r.get::<_, i64>(20)?,
        "customer_count": r.get::<_, i64>(21)?,
        "organization_count": r.get::<_, i64>(22)?,
        "owner_name": r.get::<_, Option<String>>(23)?,
    }))
}

/// Reference `listConversations`: linked, non-deleted conversations with
/// customer/mailbox labels. JSON keys keep the reference UI contract
/// (`customer_local_id`, `remote_created_at`).
#[must_use]
pub fn list_conversations(conn: &Connection, incident_id: i64, limit: i64) -> Vec<Value> {
    let sql = "SELECT ic.conversation_id, c.number, c.subject, c.status,
           (SELECT m.name FROM mailboxes m WHERE m.id = c.mailbox_local_id) AS mailbox,
           c.customer_local_id,
           (SELECT NULLIF(TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, '')), '')
              FROM customers cu WHERE cu.id = c.customer_local_id) AS customer_name,
           c.created_at, ic.linked_by, ic.linked_at
         FROM incident_conversations ic
           JOIN conversations c ON c.id = ic.conversation_id
         WHERE ic.incident_id = ?1 AND c.deleted_at IS NULL
         ORDER BY c.created_at DESC
         LIMIT ?2";
    conn.prepare(sql)
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![incident_id, limit.clamp(1, 500)], |r| {
                Ok(json!({
                    "conversation_id": r.get::<_, i64>(0)?,
                    "number": r.get::<_, i64>(1)?,
                    "subject": r.get::<_, Option<String>>(2)?,
                    "status": r.get::<_, String>(3)?,
                    "mailbox": r.get::<_, Option<String>>(4)?,
                    "customer_local_id": r.get::<_, i64>(5)?,
                    "customer_name": r.get::<_, Option<String>>(6)?,
                    "remote_created_at": r.get::<_, Option<String>>(7)?,
                    "linked_by": r.get::<_, String>(8)?,
                    "linked_at": r.get::<_, String>(9)?,
                }))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
}

/// Reference `affectedCustomers`: distinct customers derived from the
/// linked conversations — a ticket count is never used as a customer count.
#[must_use]
pub fn affected_customers(conn: &Connection, incident_id: i64, limit: i64) -> Vec<Value> {
    let sql = "SELECT c.customer_local_id,
           (SELECT NULLIF(TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, '')), '')
              FROM customers cu WHERE cu.id = c.customer_local_id) AS name,
           (SELECT cu.email FROM customers cu WHERE cu.id = c.customer_local_id) AS email,
           (SELECT NULLIF(cu.organization, '') FROM customers cu WHERE cu.id = c.customer_local_id) AS organization,
           COUNT(*) AS conversations,
           SUM(CASE WHEN c.status = 'active' THEN 1 ELSE 0 END) AS open_conversations
         FROM incident_conversations ic
           JOIN conversations c ON c.id = ic.conversation_id
         WHERE ic.incident_id = ?1 AND c.deleted_at IS NULL AND c.customer_local_id IS NOT NULL
         GROUP BY c.customer_local_id
         ORDER BY conversations DESC, c.customer_local_id
         LIMIT ?2";
    conn.prepare(sql)
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![incident_id, limit.clamp(1, 500)], |r| {
                Ok(json!({
                    "customer_local_id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, Option<String>>(1)?,
                    "email": r.get::<_, Option<String>>(2)?,
                    "organization": r.get::<_, Option<String>>(3)?,
                    "conversations": r.get::<_, i64>(4)?,
                    "open_conversations": r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                }))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
}

/// Reference `affectedOrganizations`: derived via the linked
/// conversations' customers. Port deviation: `customers.organization` is
/// free TEXT (the reference has an `organization_id` FK), so rows group by
/// the organization NAME and carry an `organization_id` only when an
/// `organizations` row with that name exists.
#[must_use]
pub fn affected_organizations(conn: &Connection, incident_id: i64, limit: i64) -> Vec<Value> {
    let sql = "SELECT (SELECT o.id FROM organizations o WHERE o.name = cu.organization LIMIT 1) AS organization_id,
           cu.organization AS name,
           COUNT(DISTINCT c.customer_local_id) AS customers,
           COUNT(*) AS conversations
         FROM incident_conversations ic
           JOIN conversations c ON c.id = ic.conversation_id
           JOIN customers cu ON cu.id = c.customer_local_id
         WHERE ic.incident_id = ?1 AND c.deleted_at IS NULL
           AND cu.organization IS NOT NULL AND TRIM(cu.organization) != ''
         GROUP BY cu.organization
         ORDER BY conversations DESC
         LIMIT ?2";
    conn.prepare(sql)
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![incident_id, limit.clamp(1, 200)], |r| {
                Ok(json!({
                    "organization_id": r.get::<_, Option<i64>>(0)?,
                    "name": r.get::<_, Option<String>>(1)?,
                    "customers": r.get::<_, i64>(2)?,
                    "conversations": r.get::<_, i64>(3)?,
                }))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
}

/// Reference `listRelated` + `labelFor`: related entities with their
/// current label (falls back to `kind #id` when the target is gone).
#[must_use]
pub fn list_related(conn: &Connection, incident_id: i64) -> Vec<Value> {
    let rows: Vec<(String, i64, Option<String>, String)> = conn
        .prepare(
            "SELECT target_kind, target_local_id, note, linked_at
             FROM incident_related WHERE incident_id = ?1 ORDER BY linked_at DESC",
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![incident_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    rows.into_iter()
        .map(|(kind, target, note, linked_at)| {
            json!({
                "incident_id": incident_id,
                "target_kind": kind,
                "target_local_id": target,
                "target_label": label_for(conn, &kind, target),
                "note": note,
                "linked_at": linked_at,
            })
        })
        .collect()
}

/// Reference `labelFor`. Port table shapes: `known_issues.name` (the
/// reference calls it `title`), `knowledge_documents.title`,
/// `outreach_campaigns.name`, `custom_objects.title`.
fn label_for(conn: &Connection, kind: &str, id: i64) -> String {
    let sql = match kind {
        "known_issue" => "SELECT name FROM known_issues WHERE id = ?1",
        "knowledge_doc" => "SELECT title FROM knowledge_documents WHERE id = ?1",
        "campaign" => "SELECT name FROM outreach_campaigns WHERE id = ?1",
        "custom_object" => "SELECT title FROM custom_objects WHERE id = ?1",
        _ => return format!("#{id}"),
    };
    let label: Option<String> = conn.query_row(sql, params![id], |r| r.get(0)).ok();
    match (kind, label) {
        ("known_issue", Some(l)) => l,
        ("known_issue", None) => format!("known issue #{id}"),
        ("knowledge_doc", Some(l)) => l,
        ("knowledge_doc", None) => format!("document #{id}"),
        ("campaign", Some(l)) => l,
        ("campaign", None) => format!("campaign #{id}"),
        ("custom_object", Some(l)) => l,
        ("custom_object", None) => format!("object #{id}"),
        _ => format!("#{id}"),
    }
}

/// Reference `listRefs`: engineering references, oldest first.
#[must_use]
pub fn list_refs(conn: &Connection, incident_id: i64) -> Vec<Value> {
    conn.prepare(
        "SELECT id, incident_id, system, reference, url, title, status, notes, created_at
         FROM incident_refs WHERE incident_id = ?1 ORDER BY id",
    )
    .and_then(|mut stmt| {
        let rows = stmt.query_map(params![incident_id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "incident_id": r.get::<_, i64>(1)?,
                "system": r.get::<_, String>(2)?,
                "reference": r.get::<_, String>(3)?,
                "url": r.get::<_, Option<String>>(4)?,
                "title": r.get::<_, Option<String>>(5)?,
                "status": r.get::<_, Option<String>>(6)?,
                "notes": r.get::<_, Option<String>>(7)?,
                "created_at": r.get::<_, String>(8)?,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap_or_default()
}

/// Reference `listReleases`: newest first.
#[must_use]
pub fn list_releases(conn: &Connection, incident_id: i64) -> Vec<Value> {
    conn.prepare(
        "SELECT id, incident_id, version_label, notes, released_at, correlation, created_at
         FROM incident_releases WHERE incident_id = ?1 ORDER BY id DESC",
    )
    .and_then(|mut stmt| {
        let rows = stmt.query_map(params![incident_id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "incident_id": r.get::<_, i64>(1)?,
                "version_label": r.get::<_, String>(2)?,
                "notes": r.get::<_, Option<String>>(3)?,
                "released_at": r.get::<_, Option<String>>(4)?,
                "correlation": r.get::<_, Option<String>>(5)?,
                "created_at": r.get::<_, String>(6)?,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap_or_default()
}

/// Reference `listNotes`: with the author label, newest first.
#[must_use]
pub fn list_notes(conn: &Connection, incident_id: i64, limit: i64) -> Vec<Value> {
    conn.prepare(
        "SELECT n.id, n.incident_id, n.author_user_local_id,
           (SELECT NULLIF(TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, '')), '')
              FROM users u WHERE u.id = n.author_user_local_id) AS author_name,
           n.body, n.created_at
         FROM incident_notes n WHERE n.incident_id = ?1
         ORDER BY n.created_at DESC LIMIT ?2",
    )
    .and_then(|mut stmt| {
        let rows = stmt.query_map(params![incident_id, limit.clamp(1, 200)], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "incident_id": r.get::<_, i64>(1)?,
                "author_user_local_id": r.get::<_, Option<i64>>(2)?,
                "author_name": r.get::<_, Option<String>>(3)?,
                "body": r.get::<_, String>(4)?,
                "created_at": r.get::<_, String>(5)?,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap_or_default()
}

/// One incident note (reference `getNote`) — for the add-note response.
#[must_use]
pub fn incident_note_json(conn: &Connection, note_id: i64) -> Option<Value> {
    conn.query_row(
        "SELECT n.id, n.incident_id, n.author_user_local_id,
           (SELECT NULLIF(TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, '')), '')
              FROM users u WHERE u.id = n.author_user_local_id) AS author_name,
           n.body, n.created_at
         FROM incident_notes n WHERE n.id = ?1",
        params![note_id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "incident_id": r.get::<_, i64>(1)?,
                "author_user_local_id": r.get::<_, Option<i64>>(2)?,
                "author_name": r.get::<_, Option<String>>(3)?,
                "body": r.get::<_, String>(4)?,
                "created_at": r.get::<_, String>(5)?,
            }))
        },
    )
    .ok()
}

/// Reference `listEvents`: the append-only workspace timeline, newest
/// first.
#[must_use]
pub fn list_events(conn: &Connection, incident_id: i64, limit: i64) -> Vec<Value> {
    conn.prepare(
        "SELECT id, incident_id, event_type, actor_user_local_id, occurred_at, detail, source
         FROM incident_events WHERE incident_id = ?1
         ORDER BY occurred_at DESC, id DESC LIMIT ?2",
    )
    .and_then(|mut stmt| {
        let rows = stmt.query_map(params![incident_id, limit.clamp(1, 500)], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "incident_id": r.get::<_, i64>(1)?,
                "event_type": r.get::<_, String>(2)?,
                "actor_user_local_id": r.get::<_, Option<i64>>(3)?,
                "occurred_at": r.get::<_, String>(4)?,
                "detail": r.get::<_, Option<String>>(5)?,
                "source": r.get::<_, String>(6)?,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn seed_mirror(conn: &Connection) {
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (10, 10, 'Support'), (20, 20, 'Billing')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO customers (id, remote_id, first_name, last_name, email, organization)
             VALUES (1, 1, 'Ada', 'Lovelace', 'ada@example.com', 'Acme'),
                    (2, 2, 'Grace', 'Hopper', 'grace@example.com', 'Acme'),
                    (3, 3, 'Edsger', 'Dijkstra', 'edsger@example.com', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_local_id, customer_local_id, created_at)
             VALUES (1, 1, 101, 'Login broken', 'active', 10, 1, '2026-10-01T10:00:00.000Z'),
                    (2, 2, 102, 'Login broken too', 'closed', 20, 2, '2026-10-02T10:00:00.000Z'),
                    (3, 3, 103, 'No org convo', 'active', 10, 3, '2026-10-03T10:00:00.000Z')",
            [],
        )
        .unwrap();
    }

    fn make_incident(conn: &Connection, conv_ids: &[i64]) -> i64 {
        let incident = crate::intelligence_features::ManualIncident {
            title: "Workspace test".into(),
            status: "investigating".into(),
            severity: "sev2".into(),
            owner_user_local_id: None,
            product: Some("Auth".into()),
            feature: Some("Login".into()),
            description: Some("Users cannot log in.".into()),
            internal_explanation: None,
            customer_safe_explanation: None,
            known_cause: None,
            workaround: None,
            resolution: None,
            started_at: None,
            conversation_ids: conv_ids.to_vec(),
        };
        crate::intelligence_features::create_manual_incident(conn, &incident).unwrap()
    }

    #[test]
    fn list_orders_open_first_and_newest() {
        let conn = fresh_db();
        seed_mirror(&conn);
        let open = make_incident(&conn, &[1]);
        let _resolved = make_incident(&conn, &[2]);
        conn.execute(
            "UPDATE incidents SET status = 'resolved', resolved_at = '2026-10-04T00:00:00.000Z' WHERE id = ?1",
            params![open + 1],
        )
        .unwrap();

        let (rows, total) = list_incident_rows(
            &conn,
            &IncidentListFilters {
                limit: 50,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
        // The open incident sorts before the resolved one even though it
        // was created first.
        assert_eq!(rows[0]["id"], json!(open));
        assert_eq!(rows[0]["status"], "investigating");
        assert_eq!(rows[1]["status"], "resolved");
    }

    #[test]
    fn list_filters_status_open_and_query() {
        let conn = fresh_db();
        seed_mirror(&conn);
        let a = make_incident(&conn, &[1]);
        let b = make_incident(&conn, &[2]);
        conn.execute(
            "UPDATE incidents SET title = 'Checkout down', code = 'INC-777' WHERE id = ?1",
            params![b],
        )
        .unwrap();
        conn.execute(
            "UPDATE incidents SET status = 'resolved' WHERE id = ?1",
            params![a],
        )
        .unwrap();

        // open only excludes resolved.
        let (rows, total) = list_incident_rows(
            &conn,
            &IncidentListFilters {
                open: true,
                limit: 50,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0]["id"], json!(b));

        // status filter.
        let (rows, total) = list_incident_rows(
            &conn,
            &IncidentListFilters {
                status: Some("resolved".into()),
                limit: 50,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0]["id"], json!(a));

        // q matches title OR code, case-insensitively via LIKE.
        for needle in ["Checkout", "INC-777", "checkout down"] {
            let (rows, total) = list_incident_rows(
                &conn,
                &IncidentListFilters {
                    query: Some(needle.into()),
                    limit: 50,
                    offset: 0,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(total, 1, "needle {needle}");
            assert_eq!(rows[0]["id"], json!(b));
        }
    }

    #[test]
    fn list_paginates() {
        let conn = fresh_db();
        seed_mirror(&conn);
        make_incident(&conn, &[]);
        make_incident(&conn, &[]);
        make_incident(&conn, &[]);
        let (page1, total) = list_incident_rows(
            &conn,
            &IncidentListFilters {
                limit: 2,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        let (page2, _) = list_incident_rows(
            &conn,
            &IncidentListFilters {
                limit: 2,
                offset: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(total, 3);
        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 1);
    }

    #[test]
    fn list_rows_carry_derived_counts() {
        let conn = fresh_db();
        seed_mirror(&conn);
        let id = make_incident(&conn, &[1, 2, 3]);
        let (rows, _) = list_incident_rows(
            &conn,
            &IncidentListFilters {
                limit: 50,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        let row = &rows[0];
        assert_eq!(row["id"], json!(id));
        assert_eq!(row["conversation_count"], 3);
        // 3 distinct customers, 2 distinct organizations (Acme x2, NULL x1).
        assert_eq!(row["customer_count"], 3);
        assert_eq!(row["organization_count"], 1);
        assert_eq!(row["product"], "Auth");
        assert_eq!(row["feature"], "Login");
        assert!(row["owner_name"].is_null());
    }

    #[test]
    fn detail_assembles_every_section() {
        let conn = fresh_db();
        seed_mirror(&conn);
        let id = make_incident(&conn, &[1, 2, 3]);

        // A related known issue with a label.
        conn.execute(
            "INSERT INTO known_issues (id, name) VALUES (5, 'OAuth token expiry')",
            [],
        )
        .unwrap();
        crate::intelligence_features::add_incident_related(
            &conn,
            id,
            "known_issue",
            5,
            Some("Root cause candidate"),
        )
        .unwrap();
        // A ref, a release, a note, and a status change (timeline event).
        crate::intelligence_features::add_incident_ref(&conn, id, "linear", "ENG-4471").unwrap();
        crate::intelligence_features::add_incident_release(
            &conn,
            id,
            "v4.12.0",
            Some("2026-10-01T00:00:00.000Z"),
            None,
        )
        .unwrap();
        crate::intelligence_features::add_incident_note(&conn, id, None, "Mitigation active.")
            .unwrap();
        conn.execute(
            "UPDATE incidents SET status = 'identified' WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let _ = crate::intelligence_features::record_incident_event(
            &conn,
            id,
            "status_changed",
            None,
            Some(&serde_json::json!({"from": "investigating", "to": "identified"})),
            &format!("incident:{id}:status:investigating:identified"),
        );

        let detail = incident_detail(&conn, id).expect("incident exists");
        assert_eq!(detail["incident"]["id"], json!(id));
        assert_eq!(detail["incident"]["title"], "Workspace test");
        assert_eq!(detail["impact"]["subject_kind"], "incident");
        assert_eq!(detail["impact"]["affected_conversations"], 3);

        let conversations = detail["conversations"].as_array().unwrap();
        assert_eq!(conversations.len(), 3);
        // Newest first; keys keep the reference UI contract.
        assert_eq!(conversations[0]["number"], 103);
        assert_eq!(
            conversations[0]["remote_created_at"],
            "2026-10-03T10:00:00.000Z"
        );
        assert_eq!(conversations[0]["customer_local_id"], 3);
        assert_eq!(conversations[0]["mailbox"], "Support");
        assert_eq!(conversations[0]["customer_name"], "Edsger Dijkstra");

        let customers = detail["affected_customers"].as_array().unwrap();
        assert_eq!(customers.len(), 3);
        // Sorted by conversation count DESC (all 1) then customer id.
        assert_eq!(customers[0]["customer_local_id"], 1);
        assert_eq!(customers[0]["name"], "Ada Lovelace");
        assert_eq!(customers[0]["email"], "ada@example.com");
        assert_eq!(customers[0]["organization"], "Acme");

        // Organizations group by name; no organizations rows exist so ids
        // are null but the name is carried.
        let orgs = detail["affected_organizations"].as_array().unwrap();
        assert_eq!(orgs.len(), 1);
        assert_eq!(orgs[0]["name"], "Acme");
        assert_eq!(orgs[0]["customers"], 2);
        assert_eq!(orgs[0]["conversations"], 2);
        assert!(orgs[0]["organization_id"].is_null());

        let related = detail["related"].as_array().unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0]["target_kind"], "known_issue");
        assert_eq!(related[0]["target_label"], "OAuth token expiry");
        assert_eq!(related[0]["note"], "Root cause candidate");

        let refs = detail["refs"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["system"], "linear");
        assert_eq!(refs[0]["reference"], "ENG-4471");

        let releases = detail["releases"].as_array().unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0]["version_label"], "v4.12.0");

        let notes = detail["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["body"], "Mitigation active.");
        assert!(notes[0]["author_name"].is_null());

        let timeline = detail["timeline"].as_array().unwrap();
        // created + 3 conversation_linked + related_linked + ref_added +
        // release_added + note_added + status_changed.
        assert_eq!(timeline.len(), 9);
        let types: Vec<&str> = timeline
            .iter()
            .map(|e| e["event_type"].as_str().unwrap())
            .collect();
        for expected in [
            "created",
            "conversation_linked",
            "related_linked",
            "ref_added",
            "release_added",
            "note_added",
            "status_changed",
        ] {
            assert!(types.contains(&expected), "missing {expected} in {types:?}");
        }
    }

    #[test]
    fn detail_missing_incident_is_none() {
        let conn = fresh_db();
        assert!(incident_detail(&conn, 999).is_none());
    }

    #[test]
    fn related_label_falls_back_when_target_is_gone() {
        let conn = fresh_db();
        let id = make_incident(&conn, &[]);
        crate::intelligence_features::add_incident_related(&conn, id, "campaign", 42, None)
            .unwrap();
        let related = list_related(&conn, id);
        assert_eq!(related[0]["target_label"], "campaign #42");
    }

    #[test]
    fn incident_note_json_round_trips() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO users (id, remote_id, first_name, last_name) VALUES (7, 7, 'Kay', 'Ng')",
            [],
        )
        .unwrap();
        let id = make_incident(&conn, &[]);
        crate::intelligence_features::add_incident_note(&conn, id, Some(7), "Note body.").unwrap();
        let notes = list_notes(&conn, id, 100);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["author_name"], "Kay Ng");
        assert_eq!(notes[0]["body"], "Note body.");
        let note_id = notes[0]["id"].as_i64().unwrap();
        assert_eq!(
            incident_note_json(&conn, note_id).unwrap()["author_name"],
            "Kay Ng"
        );
    }
}
