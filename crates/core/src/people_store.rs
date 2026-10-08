//! People store — the people-domain read + write surface on the real stores
//! (P5 part 4). Mirrors the reference `src/server/repositories/peopleRepo.ts`
//! surface the v1.x routes stubbed: organizations were a `SELECT DISTINCT
//! organization FROM customers` projection with fake `id: 0` rows, the org
//! detail route answered a hardcoded 404, both support-health routes answered
//! `"unknown"`, the org timeline was an empty array, and `POST /api/timeline/
//! rebuild` was a fake-success — while the REAL `organizations` table (FTS
//! migration), `customers.organization_id` (M036) and the whole
//! `customer_events` sweep/rebuild machinery already existed underneath.
//!
//! ## Local writes on the mirror store (documented deviation)
//!
//! The reference creates customers/organizations through the Help Scout API
//! (real remote ids) or seeds them in demo mode. The port's P5 write surfaces
//! write the mirror tables directly — the same documented deviation every P5
//! part takes (settings, incidents, knowledge). Provenance stays visible:
//! locally-created rows carry `local_created_at` timestamps and a negative
//! `remote_id` allocated from the negative space, which a Help Scout sync can
//! never produce (their ids are positive), so `ON CONFLICT(remote_id)`
//! upserts from a later sync can never overwrite a local row. Mirror rows a
//! sync brought in keep their positive remote ids untouched.
//!
//! ## Organization identity
//!
//! The mirror keeps BOTH the legacy `customers.organization` text column and
//! the M036 `customers.organization_id` FK (backfilled by name). Renaming an
//! organization updates the linked customers' text column too — otherwise
//! every name-based read (saved views, incident workspace, segment filters)
//! would silently split the org in two.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::error::Result;

// ─── Organization reads ─────────────────────────────────────────────────────

/// GET /api/organizations — every non-deleted org row with the list-page
/// payload: parsed `domains`, member `customer_count` and their
/// `conversation_count`. `q` filters by name substring (case-insensitive,
/// LIKE with escaped wildcards); `limit` caps rows (default 20, max 200).
pub fn list_organizations(
    conn: &Connection,
    query: Option<&str>,
    limit: u32,
) -> Result<Vec<Value>> {
    let limit = limit.clamp(1, 200) as i64;
    let like = query
        .map(|q| {
            format!(
                "%{}%",
                q.trim()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        })
        .unwrap_or_else(|| "%".to_string());
    let mut stmt = conn.prepare(
        "SELECT o.id, o.remote_id, o.name, o.domains, o.remote_created_at,
                o.remote_updated_at, o.local_created_at, o.local_updated_at,
                (SELECT COUNT(*) FROM customers cu
                  WHERE (cu.organization_id = o.id
                         OR (cu.organization_id IS NULL AND cu.organization = o.name))
                    AND cu.deleted_at IS NULL) AS customer_count,
                (SELECT COUNT(*) FROM conversations c
                  JOIN customers cu ON cu.id = c.customer_id
                  WHERE (cu.organization_id = o.id
                         OR (cu.organization_id IS NULL AND cu.organization = o.name))
                    AND cu.deleted_at IS NULL) AS conversation_count
         FROM organizations o
         WHERE o.deleted_at IS NULL
           AND o.name LIKE ?1 ESCAPE '\\'
         ORDER BY o.name COLLATE NOCASE
         LIMIT ?2",
    )?;
    let orgs: Vec<Value> = stmt
        .query_map(params![like, limit], org_row_to_json)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(orgs)
}

/// Map one organizations-table row to the wire payload (domains is stored as
/// a JSON array text column; parse defensively — a malformed legacy value
/// degrades to an empty array, never a 500).
fn org_row_to_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let domains_text: Option<String> = row.get("domains")?;
    let domains: Vec<String> = domains_text
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default();
    Ok(json!({
        "id": row.get::<_, i64>("id")?,
        "remote_id": row.get::<_, i64>("remote_id")?,
        "name": row.get::<_, String>("name")?,
        "domains": domains,
        "customer_count": row.get::<_, i64>("customer_count")?,
        "conversation_count": row.get::<_, i64>("conversation_count")?,
        "created_at": row
            .get::<_, Option<String>>("remote_created_at")?
            .or_else(|| row.get::<_, Option<String>>("local_created_at").ok().flatten()),
        "updated_at": row
            .get::<_, Option<String>>("remote_updated_at")?
            .or_else(|| row.get::<_, Option<String>>("local_updated_at").ok().flatten()),
    }))
}

/// The org member predicate as SQL — mirrored in list/detail queries so the
/// counts can never diverge: an FK link wins, the legacy name match covers
/// pre-M036 rows the backfill has not touched yet.
const ORG_MEMBERS_SQL: &str = "(cu.organization_id = ?1
    OR (cu.organization_id IS NULL AND cu.organization = (SELECT name FROM organizations WHERE id = ?1)))";

/// GET /api/organizations/:id — the detail payload: the org row plus its
/// member customers (id, name, email, conversation counts) and aggregate
/// stats. None when the id is unknown or soft-deleted.
pub fn get_organization_detail(conn: &Connection, org_id: i64) -> Result<Option<Value>> {
    let org: Option<Value> = {
        let mut stmt = conn.prepare(
            "SELECT o.id, o.remote_id, o.name, o.domains, o.remote_created_at,
                    o.remote_updated_at, o.local_created_at, o.local_updated_at,
                    (SELECT COUNT(*) FROM customers cu
                      WHERE (cu.organization_id = o.id
                             OR (cu.organization_id IS NULL AND cu.organization = o.name))
                        AND cu.deleted_at IS NULL) AS customer_count,
                    (SELECT COUNT(*) FROM conversations c
                      JOIN customers cu ON cu.id = c.customer_id
                      WHERE (cu.organization_id = o.id
                             OR (cu.organization_id IS NULL AND cu.organization = o.name))
                        AND cu.deleted_at IS NULL) AS conversation_count
             FROM organizations o
             WHERE o.id = ?1 AND o.deleted_at IS NULL",
        )?;
        let mut rows: Vec<Value> = stmt
            .query_map(params![org_id], org_row_to_json)?
            .filter_map(|r| r.ok())
            .collect();
        rows.pop()
    };
    let Some(mut org) = org else {
        return Ok(None);
    };

    // Members: newest activity first, capped at 100 (the detail page renders
    // a table; the full membership is the sync/segment engine's job).
    let mut stmt = conn.prepare(&format!(
        "SELECT cu.id, cu.first_name, cu.last_name, cu.email, cu.job_title,
                (SELECT COUNT(*) FROM conversations c
                  WHERE c.customer_id = cu.id AND c.deleted_at IS NULL) AS conversation_count,
                (SELECT COUNT(*) FROM conversations c
                  WHERE c.customer_id = cu.id AND c.status = 'active' AND c.deleted_at IS NULL) AS open_count,
                (SELECT MAX(COALESCE(c.updated_at, c.local_created_at)) FROM conversations c
                  WHERE c.customer_id = cu.id AND c.deleted_at IS NULL) AS last_activity_at
         FROM customers cu
         WHERE {ORG_MEMBERS_SQL} AND cu.deleted_at IS NULL
         ORDER BY COALESCE(cu.last_name, cu.last_name, cu.first_name, cu.email, cu.id)
         LIMIT 100"
    ))?;
    let members: Vec<Value> = stmt
        .query_map(params![org_id], |row| {
            let first: Option<String> = row.get(1)?;
            let last: Option<String> = row.get(2)?;
            let name = match (first.clone(), last.clone()) {
                (Some(f), Some(l)) if !f.trim().is_empty() && !l.trim().is_empty() => {
                    format!("{} {}", f.trim(), l.trim())
                }
                (Some(f), _) if !f.trim().is_empty() => f.trim().to_string(),
                (_, Some(l)) if !l.trim().is_empty() => l.trim().to_string(),
                _ => "(unnamed)".to_string(),
            };
            Ok(json!({
                "id": row.get::<_, i64>(0)?,
                "name": name,
                "email": row.get::<_, Option<String>>(3)?,
                "job_title": row.get::<_, Option<String>>(4)?,
                "conversation_count": row.get::<_, i64>(5)?,
                "open_count": row.get::<_, i64>(6)?,
                "last_activity_at": row.get::<_, Option<String>>(7)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let member_count = members.len() as i64;
    let open_total: i64 = members
        .iter()
        .filter_map(|m| m.get("open_count").and_then(|v| v.as_i64()))
        .sum();
    let conversation_total: i64 = members
        .iter()
        .filter_map(|m| m.get("conversation_count").and_then(|v| v.as_i64()))
        .sum();

    if let Some(obj) = org.as_object_mut() {
        obj.insert("members".into(), Value::Array(members));
        obj.insert("member_count".into(), json!(member_count));
        obj.insert("open_conversation_count".into(), json!(open_total));
        // Keep the SQL-derived counts authoritative where they exist.
        if obj.get("customer_count").and_then(|v| v.as_i64()) == Some(0) && member_count > 0 {
            obj.insert("customer_count".into(), json!(member_count));
        }
        if obj.get("conversation_count").and_then(|v| v.as_i64()) == Some(0)
            && conversation_total > 0
        {
            obj.insert("conversation_count".into(), json!(conversation_total));
        }
    }
    Ok(Some(org))
}

// ─── Support health (customer + organization) ───────────────────────────────

/// Evidence links are bounded (reference `EVIDENCE_LIMIT`, supportHealth.ts:19).
const EVIDENCE_LIMIT: i64 = 10;

/// Evidence conversation ids for a conv-scope predicate (reference `ev`,
/// supportHealth.ts:49-53): the newest `EVIDENCE_LIMIT` conversations the
/// predicate matches, so every metric/flag stays traceable to tickets.
fn evidence_ids(
    conn: &Connection,
    conv_scope: &str,
    subject_id: i64,
    extra_sql: &str,
    extra_params: &[rusqlite::types::Value],
) -> Vec<i64> {
    let sql = format!(
        "SELECT c.id FROM conversations c
         WHERE {conv_scope} AND c.deleted_at IS NULL {extra_sql}
         ORDER BY c.created_at DESC LIMIT {EVIDENCE_LIMIT}"
    );
    let mut params: Vec<rusqlite::types::Value> = vec![rusqlite::types::Value::Integer(subject_id)];
    params.extend_from_slice(extra_params);
    conn.prepare(&sql)
        .and_then(|mut stmt| {
            stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
                r.get::<_, i64>(0)
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
}

/// One support-health metric row (reference `SupportHealthMetric`).
fn health_metric(
    key: &str,
    label: &str,
    value: Value,
    display: String,
    definition: &str,
    evidence: Vec<i64>,
    completeness: &str,
) -> Value {
    json!({
        "key": key,
        "label": label,
        "value": value,
        "display": display,
        "definition": definition,
        "evidence_conversation_ids": evidence,
        "completeness": completeness,
    })
}

/// One support-health attention flag (reference `SupportHealthFlag`).
fn health_flag(
    key: &str,
    label: &str,
    severity: &str,
    detail: String,
    evidence: Vec<i64>,
) -> Value {
    json!({
        "key": key,
        "label": label,
        "severity": severity,
        "detail": detail,
        "evidence_conversation_ids": evidence,
    })
}

/// Deterministic support-health report for one subject (audit M22 / AN-15;
/// reference `SupportHealthService.buildReport`, supportHealth.ts:45-229):
/// operational facts ONLY — labeled metrics + explicit attention flags +
/// current incident exposure, each traceable to specific conversations.
/// There is deliberately NO single "health score": aggregating operational
/// signals into one number about a person invites psychological reading,
/// which the plan forbids.
///
/// Port schema mappings (documented renames): `c.customer_local_id` ->
/// `c.customer_id`, `c.remote_created_at` -> `c.created_at`, `threads` ->
/// `conversation_threads` (`type='customer'` -> `thread_type='customer_message'`),
/// `conversation_tags.tag_local_id` -> `conversation_tags.tag_id`,
/// `issue_cluster_conversations` -> `issue_cluster_conversations`,
/// `issue_clusters.title` -> `issue_clusters.name`,
/// `known_issue_conversations` -> `known_issue_links`.
fn build_support_health_report(
    conn: &Connection,
    subject_kind: &str,
    subject_id: i64,
    subject_label: &str,
    conv_scope: &str,
    rating_scope: &str,
    incident_scope: &str,
) -> Result<Value> {
    let mut metrics: Vec<Value> = Vec::new();
    let mut flags: Vec<Value> = Vec::new();
    let ev = |extra_sql: &str, extra: &[rusqlite::types::Value]| -> Vec<i64> {
        evidence_ids(conn, conv_scope, subject_id, extra_sql, extra)
    };

    let open_convs: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM conversations c
                 WHERE {conv_scope} AND c.deleted_at IS NULL AND c.status = 'active'"
            ),
            params![subject_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let total_convs: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM conversations c
                 WHERE {conv_scope} AND c.deleted_at IS NULL"
            ),
            params![subject_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let volume_90: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM conversations c
                 WHERE {conv_scope} AND c.deleted_at IS NULL
                   AND julianday(c.created_at) >= julianday('now', '-90 days')"
            ),
            params![subject_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    metrics.push(health_metric(
        "open_conversations",
        "Open conversations",
        json!(open_convs),
        open_convs.to_string(),
        &format!("Conversations currently in status \"active\" for this {subject_kind}."),
        ev("AND c.status = 'active'", &[]),
        "known",
    ));
    metrics.push(health_metric(
        "support_volume_total",
        "Support volume (all time)",
        json!(total_convs),
        total_convs.to_string(),
        &format!("Total conversations ever synced for this {subject_kind}."),
        ev("", &[]),
        "known",
    ));
    metrics.push(health_metric(
        "support_volume_90d",
        "Support volume (90 days)",
        json!(volume_90),
        volume_90.to_string(),
        "Conversations created in the last 90 days.",
        ev(
            "AND julianday(c.created_at) >= julianday('now', '-90 days')",
            &[],
        ),
        "known",
    ));

    // Waiting duration (calendar days; the SLA business-hours view lives on
    // the conversation itself — this is the honest subject-side number).
    let (waiting_count, waiting_avg_days, waiting_max_days): (i64, Option<f64>, Option<f64>) = conn
        .query_row(
            &format!(
                "SELECT COUNT(*),
                        AVG(julianday('now') - julianday(c.customer_waiting_since)),
                        MAX(julianday('now') - julianday(c.customer_waiting_since))
                 FROM conversations c
                 WHERE {conv_scope} AND c.deleted_at IS NULL
                   AND c.status = 'active' AND c.customer_waiting_since IS NOT NULL"
            ),
            params![subject_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((0, None, None));
    let waiting_evidence = ev(
        "AND c.status = 'active' AND c.customer_waiting_since IS NOT NULL",
        &[],
    );
    metrics.push(health_metric(
        "waiting_count",
        "Currently waiting",
        json!(waiting_count),
        waiting_count.to_string(),
        "Open conversations where the customer is waiting for a reply (local waiting marker).",
        waiting_evidence.clone(),
        if waiting_count > 0 {
            "known"
        } else {
            "unknown"
        },
    ));
    if waiting_count > 0 {
        let avg_days = waiting_avg_days.unwrap_or(0.0);
        metrics.push(health_metric(
            "waiting_avg_days",
            "Average wait",
            json!(avg_days),
            format!("{avg_days:.1} days"),
            "Average calendar days waiting across currently-open conversations (not business hours).",
            ev(
                "AND c.status = 'active' AND c.customer_waiting_since IS NOT NULL",
                &[],
            ),
            "known",
        ));
        let max_days = waiting_max_days.unwrap_or(0.0);
        if max_days >= 5.0 {
            flags.push(health_flag(
                "waiting_long",
                "Waiting a long time",
                "critical",
                format!("A conversation has been waiting {max_days:.1} calendar days for a reply."),
                waiting_evidence,
            ));
        } else if max_days >= 2.0 {
            flags.push(health_flag(
                "waiting_long",
                "Waiting",
                "warning",
                format!("A conversation has been waiting {max_days:.1} calendar days for a reply."),
                waiting_evidence,
            ));
        }
    }

    // Response delays (first response, last 90d, calendar hours).
    let (fr_avg_hours, fr_n): (Option<f64>, i64) = conn
        .query_row(
            &format!(
                "SELECT AVG((julianday(c.first_response_at) - julianday(c.first_customer_message_at)) * 24),
                        COUNT(*)
                 FROM conversations c
                 WHERE {conv_scope} AND c.deleted_at IS NULL
                   AND c.first_response_at IS NOT NULL AND c.first_customer_message_at IS NOT NULL
                   AND julianday(c.created_at) >= julianday('now', '-90 days')"
            ),
            params![subject_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((None, 0));
    let fr_avg = fr_avg_hours.unwrap_or(0.0);
    metrics.push(health_metric(
        "first_response_avg_hours",
        "Avg first response (90d)",
        json!(fr_avg),
        if fr_n > 0 {
            format!("{fr_avg:.1} hours")
        } else {
            "unknown".to_string()
        },
        "Average calendar hours between the first customer message and the first agent reply (last 90 days). Business-hours SLA timing lives on each conversation.",
        ev(
            "AND c.first_response_at IS NOT NULL AND c.first_customer_message_at IS NOT NULL AND julianday(c.created_at) >= julianday('now', '-90 days')",
            &[],
        ),
        if fr_n > 0 { "known" } else { "unknown" },
    ));

    // Negative outcomes: not-good ratings in the last 90 days.
    let bad_count: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM ratings r
                 WHERE r.rating = 'not-good' AND {rating_scope}
                   AND julianday(r.remote_created_at) >= julianday('now', '-90 days')"
            ),
            params![subject_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let bad_evidence: Vec<i64> = conn
        .prepare(&format!(
            "SELECT r.conversation_id FROM ratings r
             WHERE r.rating = 'not-good' AND {rating_scope} AND r.conversation_id IS NOT NULL
               AND julianday(r.remote_created_at) >= julianday('now', '-90 days')
             LIMIT {EVIDENCE_LIMIT}"
        ))
        .and_then(|mut stmt| {
            stmt.query_map(params![subject_id], |r| r.get::<_, i64>(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    metrics.push(health_metric(
        "negative_outcomes_90d",
        "Negative ratings (90d)",
        json!(bad_count),
        bad_count.to_string(),
        "Ratings \"not-good\" received in the last 90 days (Help Scout mirror).",
        bad_evidence.clone(),
        "known",
    ));
    if bad_count >= 2 {
        flags.push(health_flag(
            "negative_outcomes",
            "Recent negative outcomes",
            "warning",
            format!("{bad_count} \"not-good\" ratings in the last 90 days."),
            bad_evidence,
        ));
    }

    // Escalation history (escalated tag, all time).
    let escalated_sql = "AND EXISTS (SELECT 1 FROM conversation_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id AND t.name = 'escalated')";
    let escalations: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM conversations c
                 WHERE {conv_scope} AND c.deleted_at IS NULL {escalated_sql}"
            ),
            params![subject_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    metrics.push(health_metric(
        "escalation_history",
        "Escalated conversations",
        json!(escalations),
        escalations.to_string(),
        "Conversations carrying the \"escalated\" tag (all time, Help Scout mirror).",
        ev(escalated_sql, &[]),
        "known",
    ));
    if escalations >= 2 {
        flags.push(health_flag(
            "escalation_history",
            "Escalation history",
            "info",
            format!("{escalations} conversations were escalated historically."),
            ev(escalated_sql, &[]),
        ));
    }

    // Customer effort proxy: customer replies per conversation (90d).
    let avg_msgs: Option<f64> = conn
        .query_row(
            &format!(
                "SELECT AVG(x.msgs) FROM (
                   SELECT c.id,
                          (SELECT COUNT(*) FROM conversation_threads t
                            WHERE t.conversation_id = c.id AND t.thread_type = 'customer_message'
                              AND t.deleted_at IS NULL AND t.state = 'published') AS msgs
                   FROM conversations c
                   WHERE {conv_scope} AND c.deleted_at IS NULL
                     AND julianday(c.created_at) >= julianday('now', '-90 days')) x"
            ),
            params![subject_id],
            |r| r.get(0),
        )
        .unwrap_or(None);
    let avg_msgs = avg_msgs.unwrap_or(0.0);
    metrics.push(health_metric(
        "customer_effort_msgs",
        "Customer effort proxy (90d)",
        json!(avg_msgs),
        if avg_msgs > 0.0 {
            format!("{avg_msgs:.1} messages / conversation")
        } else {
            "unknown".to_string()
        },
        "Average number of customer messages per conversation in the last 90 days. A high value often means more back-and-forth to get resolved - an operational proxy, not a judgment about anyone.",
        ev("AND julianday(c.created_at) >= julianday('now', '-90 days')", &[]),
        if avg_msgs > 0.0 { "partial" } else { "unknown" },
    ));
    if avg_msgs >= 5.0 {
        flags.push(health_flag(
            "high_effort",
            "High customer effort",
            "info",
            format!(
                "An average of {avg_msgs:.1} customer messages per conversation in the last 90 days."
            ),
            ev("AND julianday(c.created_at) >= julianday('now', '-90 days')", &[]),
        ));
    }

    // Repeated issues: same cluster hitting this subject repeatedly.
    let repeated: Option<(String, i64)> = conn
        .query_row(
            &format!(
                "SELECT ic.name AS title, COUNT(*) AS n
                 FROM conversations c
                 JOIN issue_cluster_conversations icm ON icm.conversation_id = c.id
                 JOIN issue_clusters ic ON ic.id = icm.cluster_id
                 WHERE {conv_scope} AND c.deleted_at IS NULL
                 GROUP BY ic.id HAVING COUNT(*) >= 2 ORDER BY COUNT(*) DESC LIMIT 1"
            ),
            params![subject_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get(1)?,
                ))
            },
        )
        .ok();
    let repeated_n = repeated.as_ref().map_or(0, |(_, n)| *n);
    let repeated_title = repeated.as_ref().map_or(String::new(), |(t, _)| t.clone());
    metrics.push(health_metric(
        "repeated_issues",
        "Repeated issue exposure",
        json!(repeated_n),
        if repeated_n > 0 {
            format!("{repeated_n} in \"{repeated_title}\"")
        } else {
            "none".to_string()
        },
        "Most-repeated issue cluster for this subject (conversations within one cluster). \"None\" is honest when no cluster repeats.",
        ev(
            "AND EXISTS (SELECT 1 FROM issue_cluster_conversations icm WHERE icm.conversation_id = c.id)",
            &[],
        ),
        "known",
    ));
    if repeated_n >= 3 {
        flags.push(health_flag(
            "repeated_issue",
            "Repeated issue",
            "warning",
            format!("{repeated_n} conversations in the \"{repeated_title}\" issue cluster."),
            ev(
                "AND EXISTS (SELECT 1 FROM issue_cluster_conversations icm JOIN issue_clusters ic ON ic.id = icm.cluster_id WHERE icm.conversation_id = c.id AND ic.name = ?)",
                &[rusqlite::types::Value::Text(repeated_title.clone())],
            ),
        ));
    }

    // Unresolved issues: open conversations linked to non-resolved known issues.
    let unresolved_sql = "AND c.status = 'active' AND EXISTS (SELECT 1 FROM known_issue_links kil JOIN known_issues ki ON ki.id = kil.known_issue_id WHERE kil.conversation_id = c.id AND ki.status != 'resolved')";
    let unresolved_count: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM conversations c
                 WHERE {conv_scope} AND c.deleted_at IS NULL {unresolved_sql}"
            ),
            params![subject_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    metrics.push(health_metric(
        "unresolved_known_issues",
        "Open conversations on unresolved issues",
        json!(unresolved_count),
        unresolved_count.to_string(),
        "Open conversations linked to known issues that are not yet resolved.",
        ev(unresolved_sql, &[]),
        "known",
    ));
    if unresolved_count >= 1 {
        flags.push(health_flag(
            "unresolved_issues",
            "Unresolved issues",
            "warning",
            format!(
                "{unresolved_count} open conversation(s) tied to known issues that are not resolved."
            ),
            ev(unresolved_sql, &[]),
        ));
    }

    // Current incident exposure (via members for organizations).
    struct IncidentRow {
        incident_id: i64,
        code: Option<String>,
        title: Option<String>,
        severity: String,
        status: String,
        conversations: i64,
    }
    let incidents: Vec<IncidentRow> = conn
        .prepare(&format!(
            "SELECT DISTINCT i.id, i.code, i.title, i.severity, i.status, COUNT(DISTINCT c.id) AS conversations
             FROM incidents i
             JOIN incident_conversations ic ON ic.incident_id = i.id
             JOIN conversations c ON c.id = ic.conversation_id
             WHERE i.status != 'resolved' AND c.deleted_at IS NULL AND {incident_scope}
             GROUP BY i.id
             ORDER BY CASE i.severity WHEN 'sev1' THEN 1 WHEN 'sev2' THEN 2 WHEN 'sev3' THEN 3 ELSE 4 END"
        ))
        .and_then(|mut stmt| {
            stmt.query_map(params![subject_id], |r| {
                Ok(IncidentRow {
                    incident_id: r.get(0)?,
                    code: r.get(1)?,
                    title: r.get(2)?,
                    severity: r.get(3)?,
                    status: r.get(4)?,
                    conversations: r.get(5)?,
                })
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    let incident_evidence = ev(
        "AND EXISTS (SELECT 1 FROM incident_conversations ic JOIN incidents i ON i.id = ic.incident_id WHERE ic.conversation_id = c.id AND i.status != 'resolved')",
        &[],
    );
    metrics.push(health_metric(
        "incident_exposure",
        "Current incident exposure",
        json!(incidents.len()),
        if incidents.is_empty() {
            "none".to_string()
        } else {
            format!("{} active incident(s)", incidents.len())
        },
        "Active (non-resolved) incidents this subject is exposed to via linked conversations.",
        incident_evidence,
        "known",
    ));
    let incident_exposure: Vec<Value> = incidents
        .iter()
        .map(|inc| {
            let severity = match inc.severity.as_str() {
                "sev1" => "critical",
                "sev2" => "warning",
                _ => "info",
            };
            flags.push(health_flag(
                &format!("incident_{}", inc.incident_id),
                &format!("Incident {}", inc.code.clone().unwrap_or_default()),
                severity,
                format!(
                    "{} linked conversation(s) in active incident \"{}\" (status {}).",
                    inc.conversations,
                    inc.title.clone().unwrap_or_default(),
                    inc.status
                ),
                ev(
                    "AND EXISTS (SELECT 1 FROM incident_conversations ic JOIN incidents i ON i.id = ic.incident_id WHERE ic.conversation_id = c.id AND i.status != 'resolved' AND i.id = ?)",
                    &[rusqlite::types::Value::Integer(inc.incident_id)],
                ),
            ));
            json!({
                "incident_id": inc.incident_id,
                "code": inc.code,
                "title": inc.title,
                "severity": inc.severity,
                "status": inc.status,
                "conversations": inc.conversations,
            })
        })
        .collect();

    Ok(json!({
        "subject_kind": subject_kind,
        "subject_id": subject_id,
        "subject_label": subject_label,
        "metrics": metrics,
        "flags": flags,
        "incident_exposure": incident_exposure,
        "generated_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        "note": "Operational facts with evidence only. No psychological or personal judgments; no aggregate \"score\" by design (plan Phase 24).",
    }))
}

// ─── Customer reads (UI-04: the reference peopleRepo customer surface) ─────

/// GET /api/customers — the paginated customer summary list (reference
/// `peopleRepo.listCustomers` + `CustomerSummary`):
/// name/email search with ESCAPED LIKE wildcards (the v2.2.1 audit rule —
/// '50%' or 'a_b' must match literally), real `total` from the SAME where
/// clause, and per-row conversation/open/last-activity/average-rating
/// aggregates. `q` also matches the customer_emails table (Help Scout
/// identity linking) exactly like the reference.
///
/// The port serves `email`/`phone` from the flat legacy columns when the
/// mirror tables have no rows for the customer (documented deviation: the
/// port's customers table pre-dates the per-value mirror tables).
pub fn list_customers_summary(
    conn: &Connection,
    query: Option<&str>,
    page: i64,
    page_size: i64,
) -> Result<(Vec<Value>, i64)> {
    let page = page.clamp(1, 100_000);
    let page_size = page_size.clamp(1, 200);
    let offset = (page - 1) * page_size;

    let (where_sql, like): (String, Option<String>) = match query {
        Some(q) if !q.trim().is_empty() => {
            let like = format!(
                "%{}%",
                q.trim()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            (
                "WHERE (c.first_name LIKE ?1 ESCAPE '\\'
                    OR c.last_name LIKE ?1 ESCAPE '\\'
                    OR c.email LIKE ?1 ESCAPE '\\'
                    OR c.organization LIKE ?1 ESCAPE '\\'
                    OR EXISTS (SELECT 1 FROM customer_emails ce
                                WHERE ce.customer_id = c.id
                                  AND ce.value LIKE ?1 ESCAPE '\\'))
                  AND c.deleted_at IS NULL"
                    .to_string(),
                Some(like),
            )
        }
        _ => ("WHERE c.deleted_at IS NULL".to_string(), None),
    };

    // Real total under the same predicate (the reference counts, then pages).
    let total: i64 = if let Some(ref like) = like {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM customers c {where_sql}"),
            params![like],
            |r| r.get(0),
        )
        .unwrap_or(0)
    } else {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM customers c {where_sql}"),
            [],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };

    let mut stmt = conn.prepare(&format!(
        "SELECT c.id, c.remote_id, c.first_name, c.last_name, c.job_title,
                c.email, c.phone, c.organization, c.organization_id,
                (SELECT o.name FROM organizations o WHERE o.id = c.organization_id) AS organization_name,
                (SELECT GROUP_CONCAT(ce.value) FROM customer_emails ce WHERE ce.customer_id = c.id) AS emails_csv,
                (SELECT GROUP_CONCAT(cp.value) FROM customer_phones cp WHERE cp.customer_id = c.id) AS phones_csv,
                (SELECT COUNT(*) FROM conversations cv
                  WHERE cv.customer_id = c.id AND cv.deleted_at IS NULL) AS conversation_count,
                (SELECT COUNT(*) FROM conversations cv
                  WHERE cv.customer_id = c.id AND cv.status IN ('active','pending')
                    AND cv.deleted_at IS NULL) AS open_conversation_count,
                (SELECT MAX(cv.last_activity_at) FROM conversations cv
                  WHERE cv.customer_id = c.id) AS last_activity_at,
                (SELECT AVG(CASE r.rating WHEN 'great' THEN 5 WHEN 'okay' THEN 3 WHEN 'not-good' THEN 1 END)
                   FROM ratings r WHERE r.customer_local_id = c.id) AS average_rating
         FROM customers c
         {where_sql}
         ORDER BY COALESCE(
             (SELECT MAX(cv.last_activity_at) FROM conversations cv WHERE cv.customer_id = c.id),
             c.local_created_at,
             c.created_at) DESC
         LIMIT ?2 OFFSET ?3"
    ))?;

    // The statement numbers its bind slots from ?1 (the LIKE term when `q`
    // is present; an unused placeholder otherwise) through ?3 — always
    // bind all three so the indexes line up in both branches.
    let like_param = like.clone().unwrap_or_else(|| "%".to_string());
    let rows: Vec<Value> = stmt
        .query_map(params![like_param, page_size, offset], customer_summary_row)?
        .filter_map(|r| r.ok())
        .collect();
    Ok((rows, total))
}

/// Map one list row to the wire `CustomerSummary` (reference shape). The
/// GROUP_CONCAT fallbacks keep the legacy flat `email`/`phone` columns
/// visible when the mirror tables have no rows (documented deviation).
fn customer_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let flat_email: Option<String> = row.get("email")?;
    let flat_phone: Option<String> = row.get("phone")?;
    let org_name: Option<String> = row.get("organization_name")?;
    let legacy_org: Option<String> = row.get("organization")?;
    let emails_csv: Option<String> = row.get("emails_csv")?;
    let phones_csv: Option<String> = row.get("phones_csv")?;
    let average_rating: Option<f64> = row.get("average_rating")?;
    Ok(json!({
        "id": row.get::<_, i64>("id")?,
        "remote_id": row.get::<_, i64>("remote_id")?,
        "first_name": row.get::<_, Option<String>>("first_name")?,
        "last_name": row.get::<_, Option<String>>("last_name")?,
        "job_title": row.get::<_, Option<String>>("job_title")?,
        "emails": csv_or_legacy(emails_csv, flat_email),
        "phones": csv_or_legacy(phones_csv, flat_phone),
        "organization_id": row.get::<_, Option<i64>>("organization_id")?,
        "organization_name": org_name.or(legacy_org),
        "conversation_count": row.get::<_, i64>("conversation_count")?,
        "open_conversation_count": row.get::<_, i64>("open_conversation_count")?,
        "last_activity_at": row.get::<_, Option<String>>("last_activity_at")?,
        "average_rating": average_rating,
    }))
}

/// GROUP_CONCAT of the mirror table, falling back to the legacy flat column
/// (both empty → the empty array the reference's GROUP_CONCAT produces).
fn csv_or_legacy(csv: Option<String>, legacy: Option<String>) -> Vec<String> {
    let from_csv = csv
        .as_deref()
        .map(|c| {
            c.split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !from_csv.is_empty() {
        return from_csv;
    }
    legacy.map(|e| vec![e]).unwrap_or_default()
}

/// GET /api/customers/:id — the reference `CustomerDetailData` envelope:
/// `{customer, conversations, ratings, memories, properties, websites,
/// social_profiles, address, topics, resolutions}`.
///
/// The customer object carries the summary aggregates (`open_conversation_count`,
/// `average_rating`, `emails[]`, `phones[]`, `organization_name`) like the
/// reference `getCustomerByLocalId` → `getCustomerByRemoteId` chain.
/// `None` when the id is unknown or soft-deleted.
pub fn get_customer_detail(conn: &Connection, customer_id: i64) -> Result<Option<Value>> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM customers WHERE id = ?1 AND deleted_at IS NULL",
            params![customer_id],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        return Ok(None);
    }

    // Customer summary (same shape as the list rows).
    let mut stmt = conn.prepare(
        "SELECT c.id, c.remote_id, c.first_name, c.last_name, c.job_title,
                c.email, c.phone, c.organization, c.organization_id,
                (SELECT o.name FROM organizations o WHERE o.id = c.organization_id) AS organization_name,
                (SELECT GROUP_CONCAT(ce.value) FROM customer_emails ce WHERE ce.customer_id = c.id) AS emails_csv,
                (SELECT GROUP_CONCAT(cp.value) FROM customer_phones cp WHERE cp.customer_id = c.id) AS phones_csv,
                (SELECT COUNT(*) FROM conversations cv
                  WHERE cv.customer_id = c.id AND cv.deleted_at IS NULL) AS conversation_count,
                (SELECT COUNT(*) FROM conversations cv
                  WHERE cv.customer_id = c.id AND cv.status IN ('active','pending')
                    AND cv.deleted_at IS NULL) AS open_conversation_count,
                (SELECT MAX(cv.last_activity_at) FROM conversations cv
                  WHERE cv.customer_id = c.id) AS last_activity_at,
                (SELECT AVG(CASE r.rating WHEN 'great' THEN 5 WHEN 'okay' THEN 3 WHEN 'not-good' THEN 1 END)
                   FROM ratings r WHERE r.customer_local_id = c.id) AS average_rating
         FROM customers c WHERE c.id = ?1",
    )?;
    let customer = stmt
        .query_row(params![customer_id], customer_summary_row)
        .map_err(crate::error::Error::from)?;

    // Conversations (reference: 50 newest with assignee names).
    let mut stmt = conn.prepare(
        "SELECT cv.id, cv.number, cv.subject, cv.status, cv.preview,
                cv.remote_created_at, cv.closed_at, cv.assignee_id,
                TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, '')) AS assignee
         FROM conversations cv
         LEFT JOIN users u ON u.id = cv.assignee_id
         WHERE cv.customer_id = ?1 AND cv.deleted_at IS NULL
         ORDER BY COALESCE(cv.remote_created_at, cv.created_at) DESC LIMIT 50",
    )?;
    let conversations: Vec<Value> = stmt
        .query_map(params![customer_id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "number": r.get::<_, i64>(1)?,
                "subject": r.get::<_, Option<String>>(2)?,
                "status": r.get::<_, String>(3)?,
                "preview": r.get::<_, Option<String>>(4)?,
                "remote_created_at": r.get::<_, Option<String>>(5)?,
                "closed_at": r.get::<_, Option<String>>(6)?,
                "assignee": r.get::<_, Option<String>>(8)?.filter(|s| !s.trim().is_empty()),
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Ratings (reference getRatingsForCustomer: newest first).
    let mut stmt = conn.prepare(
        "SELECT rating, comments, remote_created_at, conversation_id
         FROM ratings WHERE customer_local_id = ?1
         ORDER BY remote_created_at DESC LIMIT 50",
    )?;
    let ratings: Vec<Value> = stmt
        .query_map(params![customer_id], |r| {
            Ok(json!({
                "rating": r.get::<_, Option<String>>(0)?,
                "comments": r.get::<_, Option<String>>(1)?,
                "created_at": r.get::<_, Option<String>>(2)?,
                "conversation_id": r.get::<_, Option<i64>>(3)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Memories (reference aiRepo.getMemories — the port's customer_memory
    // table; the composed ME-01 profile lives at /api/memory/:id).
    let mut stmt = conn.prepare(
        "SELECT id, memory_key, memory_value, source, confidence, last_seen_at
         FROM customer_memory WHERE customer_id = ?1
         ORDER BY COALESCE(last_seen_at, created_at) DESC LIMIT 50",
    )?;
    let memories: Vec<Value> = stmt
        .query_map(params![customer_id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "key": r.get::<_, String>(1)?,
                "value": r.get::<_, String>(2)?,
                "source": r.get::<_, String>(3)?,
                "confidence": r.get::<_, Option<String>>(4)?,
                "last_seen_at": r.get::<_, Option<String>>(5)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Properties (reference getCustomerProperties — definition names).
    let mut stmt = conn.prepare(
        "SELECT d.name, cp.value
         FROM customer_properties cp
         LEFT JOIN customer_property_definitions d ON d.id = cp.definition_id
         WHERE cp.customer_id = ?1
         ORDER BY 1 LIMIT 50",
    )?;
    let properties: Vec<Value> = stmt
        .query_map(params![customer_id], |r| {
            Ok(json!({
                "name": r.get::<_, String>(0)?,
                "value": r.get::<_, Option<String>>(1)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Websites / social profiles / address (the mirror tables; empty arrays
    // when the sync has not brought any).
    let websites: Vec<Value> = string_column(
        conn,
        "SELECT value FROM customer_websites WHERE customer_id = ?1 ORDER BY id LIMIT 20",
        customer_id,
    )?;
    let social_profiles: Vec<Value> = conn
        .prepare(
            "SELECT type, value FROM customer_social_profiles
             WHERE customer_id = ?1 ORDER BY id LIMIT 20",
        )?
        .query_map(params![customer_id], |r| {
            Ok(json!({
                "type": r.get::<_, Option<String>>(0)?,
                "value": r.get::<_, Option<String>>(1)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();
    let address: Option<String> = conn
        .prepare(
            "SELECT lines, city, state, postal_code, country
             FROM customer_addresses WHERE customer_id = ?1 ORDER BY id LIMIT 1",
        )?
        .query_row(params![customer_id], |r| {
            let parts = [
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ];
            let joined = parts
                .iter()
                .flatten()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            Ok(if joined.is_empty() {
                None
            } else {
                Some(joined)
            })
        })
        .unwrap_or(None);

    // Recent topics (reference: subjects of the 10 newest conversations).
    let topics: Vec<Value> = conversations
        .iter()
        .take(10)
        .map(|c| {
            json!({
                "number": c.get("number").cloned().unwrap_or(Value::Null),
                "topic": c.get("subject").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    // Previous resolutions (reference: last published reply per closed
    // conversation, newest close first, 5 rows, resolution truncated to 400).
    let mut stmt = conn.prepare(
        "SELECT cv.number, cv.subject,
                (SELECT t.body FROM conversation_threads t
                  WHERE t.conversation_id = cv.id AND t.thread_type = 'reply'
                    AND t.state = 'published'
                  ORDER BY COALESCE(t.remote_created_at, t.created_at) DESC LIMIT 1) AS resolution,
                cv.closed_at
         FROM conversations cv
         WHERE cv.customer_id = ?1 AND cv.status = 'closed' AND cv.deleted_at IS NULL
         ORDER BY cv.closed_at DESC LIMIT 5",
    )?;
    let resolutions: Vec<Value> = stmt
        .query_map(params![customer_id], |r| {
            let resolution: Option<String> = r.get(2)?;
            Ok(json!({
                "number": r.get::<_, i64>(0)?,
                "subject": r.get::<_, Option<String>>(1)?,
                "resolution": resolution.map(|res| res.chars().take(400).collect::<String>()),
                "closed_at": r.get::<_, Option<String>>(3)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Some(json!({
        "customer": customer,
        "conversations": conversations,
        "ratings": ratings,
        "memories": memories,
        "properties": properties,
        "websites": websites,
        "social_profiles": social_profiles,
        "address": address,
        "topics": topics,
        "resolutions": resolutions,
    })))
}

/// Read a single TEXT column into JSON strings (websites list helper).
fn string_column(conn: &Connection, sql: &str, id: i64) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<Value> = stmt
        .query_map(params![id], |r| Ok(json!(r.get::<_, Option<String>>(0)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Support health for one customer (audit M22 / AN-15; reference
/// `SupportHealthService.forCustomer`, supportHealth.ts:25-32). `None` when
/// the id is unknown or soft-deleted.
pub fn customer_support_health(conn: &Connection, customer_id: i64) -> Result<Option<Value>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, TRIM(COALESCE(first_name, '') || ' ' || COALESCE(last_name, ''))
             FROM customers WHERE id = ?1 AND deleted_at IS NULL",
            params![customer_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((id, raw_label)) = row else {
        return Ok(None);
    };
    let label = raw_label.trim();
    let label = if label.is_empty() {
        format!("customer #{customer_id}")
    } else {
        label.to_string()
    };
    build_support_health_report(
        conn,
        "customer",
        id,
        &label,
        "c.customer_id = ?1",
        "r.customer_local_id = ?1",
        "c.customer_id = ?1",
    )
    .map(Some)
}

/// Support health for one organization (reference
/// `SupportHealthService.forOrganization`, supportHealth.ts:34-43): the same
/// report over the union of all member conversations. `None` when the id is
/// unknown or soft-deleted.
pub fn organization_support_health(conn: &Connection, org_id: i64) -> Result<Option<Value>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, name FROM organizations WHERE id = ?1 AND deleted_at IS NULL",
            params![org_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((id, raw_name)) = row else {
        return Ok(None);
    };
    let name = raw_name.trim();
    let label = if name.is_empty() {
        format!("organization #{org_id}")
    } else {
        name.to_string()
    };
    // Members subselect (the port's organization linkage: organization_id
    // where present, the legacy organization name otherwise).
    let members = format!(
        "(SELECT cu.id FROM customers cu WHERE {ORG_MEMBERS_SQL} AND cu.deleted_at IS NULL)"
    );
    let conv_scope = format!("c.customer_id IN {members}");
    let rating_scope = format!("r.customer_local_id IN {members}");
    build_support_health_report(
        conn,
        "organization",
        id,
        &label,
        &conv_scope,
        &rating_scope,
        &conv_scope,
    )
    .map(Some)
}

// ─── Customer writes ────────────────────────────────────────────────────────

/// Fields for a validated customer create. The route owns zod-parity
/// validation; the store only receives clean values.
pub struct NewCustomer {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
}

/// Fields for a validated customer PATCH. `None` = field absent from the
/// request (leave untouched); `Some(None)` = explicit null (clear).
#[derive(Default)]
pub struct CustomerPatch {
    pub first_name: Option<Option<String>>,
    pub last_name: Option<Option<String>>,
    pub email: Option<Option<String>>,
    pub organization: Option<Option<String>>,
    pub job_title: Option<Option<String>>,
    pub phone: Option<Option<String>>,
}

impl CustomerPatch {
    pub fn is_empty(&self) -> bool {
        self.first_name.is_none()
            && self.last_name.is_none()
            && self.email.is_none()
            && self.organization.is_none()
            && self.job_title.is_none()
            && self.phone.is_none()
    }
}

/// Allocate the next negative remote_id — the local-write space a Help Scout
/// sync can never collide with (their ids are strictly positive). Only
/// existing NEGATIVE ids are considered, so a mirror full of large positive
/// sync ids never pushes the allocation back into collision territory.
fn next_local_remote_id(conn: &Connection, table: &str) -> Result<i64> {
    let next: i64 = conn.query_row(
        &format!("SELECT COALESCE(MIN(remote_id), 0) - 1 FROM {table} WHERE remote_id < 0"),
        [],
        |r| r.get(0),
    )?;
    debug_assert!(next < 0, "local remote_id allocation must stay negative");
    Ok(next)
}

/// POST /api/customers — insert the customer, resolve the organization FK
/// by name (creating nothing: an unknown org name stays a text column, the
/// same state pre-M036 sync data lives in), and index the email/phone into
/// their mirror side tables so search behaves like a synced customer.
/// Returns the new local id.
pub fn create_customer(conn: &Connection, new: &NewCustomer) -> Result<i64> {
    let remote_id = next_local_remote_id(conn, "customers")?;
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    conn.execute(
        "INSERT INTO customers
           (remote_id, first_name, last_name, email, organization, organization_id,
            job_title, phone, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5,
                 (SELECT o.id FROM organizations o
                   WHERE o.name = ?5 AND o.deleted_at IS NULL LIMIT 1),
                 ?6, ?7, ?8, ?8)",
        params![
            remote_id,
            new.first_name,
            new.last_name,
            new.email,
            new.organization,
            new.job_title,
            new.phone,
            now
        ],
    )?;
    let id = conn.last_insert_rowid();
    if let Some(email) = new.email.as_deref().filter(|e| !e.trim().is_empty()) {
        let _ = conn.execute(
            "INSERT OR IGNORE INTO customer_emails (customer_id, value, type)
             VALUES (?1, ?2, 'work')",
            params![id, email.trim()],
        );
    }
    if let Some(phone) = new.phone.as_deref().filter(|p| !p.trim().is_empty()) {
        let _ = conn.execute(
            "INSERT OR IGNORE INTO customer_phones (customer_id, value, type)
             VALUES (?1, ?2, 'work')",
            params![id, phone.trim()],
        );
    }
    let _ = crate::audit::audit(
        conn,
        &crate::audit::AuditEntry::user("customer_created").with_after_state(json!({
            "customer_id": id,
            "remote_id": remote_id,
            "email": new.email,
            "organization": new.organization,
        })),
    );
    Ok(id)
}

/// PATCH /api/customers/:id — apply the patch, bump `updated_at`, re-resolve
/// the organization FK when the text column changed, and keep the email side
/// table in step (the email the customer is searched by). Returns false when
/// the id is unknown or soft-deleted.
pub fn update_customer(conn: &Connection, customer_id: i64, patch: &CustomerPatch) -> Result<bool> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM customers WHERE id = ?1 AND deleted_at IS NULL",
            params![customer_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        return Ok(false);
    }
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    // PATCH semantics need to distinguish absent (untouched) from explicit
    // null (cleared) — a COALESCE(?n, col) pattern cannot — so the SET
    // clause is built from exactly the fields the patch carries.
    let mut sets: Vec<&'static str> = Vec::new();
    let mut binds: Vec<Option<String>> = Vec::new();
    if let Some(v) = &patch.first_name {
        sets.push("first_name = ?");
        binds.push(v.clone());
    }
    if let Some(v) = &patch.last_name {
        sets.push("last_name = ?");
        binds.push(v.clone());
    }
    if let Some(v) = &patch.email {
        sets.push("email = ?");
        binds.push(v.clone());
    }
    if let Some(v) = &patch.organization {
        sets.push("organization = ?");
        binds.push(v.clone());
    }
    if let Some(v) = &patch.job_title {
        sets.push("job_title = ?");
        binds.push(v.clone());
    }
    if let Some(v) = &patch.phone {
        sets.push("phone = ?");
        binds.push(v.clone());
    }
    sets.push("updated_at = ?");
    binds.push(Some(now));
    let sql = format!("UPDATE customers SET {} WHERE id = ?", sets.join(", "));
    let mut all_binds = binds;
    all_binds.push(Some(customer_id.to_string()));
    conn.execute(
        &sql,
        rusqlite::params_from_iter(all_binds.iter().map(|v| v.as_deref())),
    )?;
    // Re-resolve the organization FK whenever the text column was written
    // (set OR cleared): a resolvable name links, an empty name unlinks. An
    // untouched patch leaves a sync-provided FK alone.
    if patch.organization.is_some() {
        conn.execute(
            "UPDATE customers SET organization_id = (
                SELECT o.id FROM organizations o
                 WHERE o.name = customers.organization AND o.deleted_at IS NULL LIMIT 1)
             WHERE id = ?1",
            params![customer_id],
        )?;
    }
    // Keep the searchable email side table in step: a changed email replaces
    // the old row (search by email finds the NEW value); a cleared email
    // removes it. Absent = untouched.
    if let Some(email) = patch.email.as_ref() {
        let _ = conn.execute(
            "DELETE FROM customer_emails WHERE customer_id = ?1",
            params![customer_id],
        );
        if let Some(e) = email.as_deref().filter(|e| !e.trim().is_empty()) {
            let _ = conn.execute(
                "INSERT OR IGNORE INTO customer_emails (customer_id, value, type)
                 VALUES (?1, ?2, 'work')",
                params![customer_id, e.trim()],
            );
        }
    }
    let changed_fields: Vec<Value> = [
        "first_name",
        "last_name",
        "email",
        "organization",
        "job_title",
        "phone",
    ]
    .into_iter()
    .filter(|f| patch_field_present(patch, f))
    .map(Value::from)
    .collect();
    let _ = crate::audit::audit(
        conn,
        &crate::audit::AuditEntry::user("customer_updated").with_after_state(json!({
            "customer_id": customer_id,
            "fields": changed_fields,
        })),
    );
    Ok(true)
}

/// Whether a named field is present in the patch (audit helper).
fn patch_field_present(patch: &CustomerPatch, field: &str) -> bool {
    match field {
        "first_name" => patch.first_name.is_some(),
        "last_name" => patch.last_name.is_some(),
        "email" => patch.email.is_some(),
        "organization" => patch.organization.is_some(),
        "job_title" => patch.job_title.is_some(),
        "phone" => patch.phone.is_some(),
        _ => false,
    }
}

// ─── Organization writes ────────────────────────────────────────────────────

/// POST /api/organizations — insert the org with its domains (stored as a
/// JSON array text column, the sync engine's format). Returns the new id.
pub fn create_organization(conn: &Connection, name: &str, domains: &[String]) -> Result<i64> {
    let remote_id = next_local_remote_id(conn, "organizations")?;
    let domains_json = serde_json::to_string(domains).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO organizations (remote_id, name, domains, last_synced_at)
         VALUES (?1, ?2, ?3, datetime('now'))",
        params![remote_id, name, domains_json],
    )?;
    let id = conn.last_insert_rowid();
    let _ = crate::audit::audit(
        conn,
        &crate::audit::AuditEntry::user("organization_created").with_after_state(json!({
            "organization_id": id,
            "remote_id": remote_id,
            "name": name,
            "domains": domains,
        })),
    );
    Ok(id)
}

/// PATCH /api/organizations/:id — rename and/or replace domains. A rename
/// also updates the legacy `customers.organization` text on linked rows so
/// name-based reads (saved views, segments, incident workspace) keep seeing
/// one org, not two. Returns false when the id is unknown or soft-deleted.
pub fn update_organization(
    conn: &Connection,
    org_id: i64,
    name: Option<&str>,
    domains: Option<&[String]>,
) -> Result<bool> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM organizations WHERE id = ?1 AND deleted_at IS NULL",
            params![org_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        return Ok(false);
    }
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    if let Some(domains) = domains {
        let domains_json = serde_json::to_string(domains).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "UPDATE organizations SET domains = ?2, local_updated_at = ?3 WHERE id = ?1",
            params![org_id, domains_json, now],
        )?;
    }
    if let Some(new_name) = name {
        conn.execute(
            "UPDATE organizations SET name = ?2, local_updated_at = ?3 WHERE id = ?1",
            params![org_id, new_name, now],
        )?;
        // Keep the legacy text column on linked rows in step (FK-linked
        // first, then the pre-M036 name matches that now resolve to us).
        conn.execute(
            "UPDATE customers SET organization = ?2, updated_at = ?3
             WHERE organization_id = ?1
                OR (organization_id IS NULL AND organization = (SELECT name FROM organizations WHERE id = ?1))",
            params![org_id, new_name, now],
        )?;
    }
    let _ = crate::audit::audit(
        conn,
        &crate::audit::AuditEntry::user("organization_updated").with_after_state(json!({
            "organization_id": org_id,
            "name": name,
            "domains": domains,
        })),
    );
    Ok(true)
}

// ─── Timeline rebuild ───────────────────────────────────────────────────────

/// POST /api/timeline/rebuild — run the real customer-events rebuild (the
/// sweep that derives `signup`/`support_conversation`/`rating`/`incident_
/// exposure` events from the mirror data) and re-resolve the organization
/// FK backfill (a port-side nicety that keeps org timelines resolvable; it
/// stays off the wire). Returns the count of NEW events derived — the
/// reference's `created`. The `customer_events_rebuilt` audit row is
/// written by the route, exactly like the reference (one row, one shape).
pub fn timeline_rebuild(conn: &Connection) -> Result<usize> {
    let events = crate::customer_events::rebuild(conn)?;
    conn.execute(
        "UPDATE customers SET organization_id = (
            SELECT o.id FROM organizations o
             WHERE o.name = customers.organization AND o.deleted_at IS NULL LIMIT 1)
         WHERE organization_id IS NULL AND organization IS NOT NULL AND organization != ''",
        [],
    )?;
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    #[test]
    fn organizations_start_empty_and_create_works() {
        let conn = fresh_db();
        let orgs = list_organizations(&conn, None, 50).unwrap();
        assert!(orgs.is_empty());

        let id = create_organization(&conn, "Acme Corp", &["acme.com".into()]).unwrap();
        let orgs = list_organizations(&conn, None, 50).unwrap();
        assert_eq!(orgs.len(), 1);
        assert_eq!(orgs[0]["name"], "Acme Corp");
        assert_eq!(orgs[0]["domains"], json!(["acme.com"]));
        assert_eq!(orgs[0]["id"], json!(id));

        // Detail payload carries members + stats.
        let detail = get_organization_detail(&conn, id).unwrap().unwrap();
        assert_eq!(detail["member_count"], json!(0));
        assert_eq!(detail["conversation_count"], json!(0));
        assert!(get_organization_detail(&conn, id + 999).unwrap().is_none());
    }

    #[test]
    fn customer_create_resolves_org_fk_and_indexes_email() {
        let conn = fresh_db();
        let org_id = create_organization(&conn, "Acme", &[]).unwrap();
        let cid = create_customer(
            &conn,
            &NewCustomer {
                first_name: Some("Ada".into()),
                last_name: Some("Lovelace".into()),
                email: Some("ada@acme.com".into()),
                organization: Some("Acme".into()),
                job_title: Some("Engineer".into()),
                phone: None,
            },
        )
        .unwrap();
        // FK resolved by name.
        let org_fk: Option<i64> = conn
            .query_row(
                "SELECT organization_id FROM customers WHERE id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(org_fk, Some(org_id));
        // Email indexed into the side table.
        let email: String = conn
            .query_row(
                "SELECT value FROM customer_emails WHERE customer_id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(email, "ada@acme.com");
        // Local remote_id is negative (sync-safe space).
        let remote: i64 = conn
            .query_row(
                "SELECT remote_id FROM customers WHERE id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert!(remote < 0);
    }

    #[test]
    fn customer_patch_semantics_present_null_absent() {
        let conn = fresh_db();
        let cid = create_customer(
            &conn,
            &NewCustomer {
                first_name: Some("Grace".into()),
                last_name: Some("Hopper".into()),
                email: Some("grace@navy.mil".into()),
                organization: None,
                job_title: Some("Rear Admiral".into()),
                phone: None,
            },
        )
        .unwrap();
        // Present-null clears; absent untouched.
        let patch = CustomerPatch {
            email: Some(None),
            job_title: Some(Some("Mathematician".into())),
            ..Default::default()
        };
        assert!(update_customer(&conn, cid, &patch).unwrap());
        let (email, job): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT email, job_title FROM customers WHERE id = ?1",
                params![cid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(email, None);
        assert_eq!(job.as_deref(), Some("Mathematician"));
        // The cleared email is gone from the side table too.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM customer_emails WHERE customer_id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        // Unknown id: false, not an error.
        assert!(!update_customer(&conn, cid + 999, &patch).unwrap());
    }

    #[test]
    fn org_rename_keeps_linked_customers_in_step() {
        let conn = fresh_db();
        let org_id = create_organization(&conn, "Acme", &[]).unwrap();
        let cid = create_customer(
            &conn,
            &NewCustomer {
                first_name: Some("Ada".into()),
                last_name: None,
                email: None,
                organization: Some("Acme".into()),
                job_title: None,
                phone: None,
            },
        )
        .unwrap();
        assert!(update_organization(&conn, org_id, Some("Acme Corp"), None).unwrap());
        let (text, _fk): (String, i64) = conn
            .query_row(
                "SELECT organization, organization_id FROM customers WHERE id = ?1",
                params![cid],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get(1)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(text, "Acme Corp");
        // Detail still finds the member after the rename.
        let detail = get_organization_detail(&conn, org_id).unwrap().unwrap();
        assert_eq!(detail["member_count"], json!(1));
        assert_eq!(detail["members"][0]["name"], "Ada");
    }

    #[test]
    fn support_health_metrics_flags_and_incidents() {
        let conn = fresh_db();
        let cid = create_customer(
            &conn,
            &NewCustomer {
                first_name: Some("No".into()),
                last_name: Some("Conversations".into()),
                email: None,
                organization: None,
                job_title: None,
                phone: None,
            },
        )
        .unwrap();
        // No conversations: the full metric set is present with zeros, no
        // flags, no incident exposure — and NO "health" verdict key (the
        // plan-forbidden single score).
        let h = customer_support_health(&conn, cid).unwrap().unwrap();
        assert_eq!(h["subject_kind"], "customer");
        assert_eq!(h["subject_id"], json!(cid));
        assert_eq!(h["subject_label"], "No Conversations");
        assert!(h.get("health").is_none(), "no verdict key by design: {h:?}");
        assert!(h["note"].as_str().unwrap().contains("no aggregate"));
        assert!(h["generated_at"].as_str().is_some_and(|t| t.ends_with('Z')));
        let metric = |key: &str| {
            h["metrics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["key"] == json!(key))
                .unwrap_or_else(|| panic!("missing metric {key}: {}", h["metrics"]))
                .clone()
        };
        for key in [
            "open_conversations",
            "support_volume_total",
            "support_volume_90d",
            "waiting_count",
            "first_response_avg_hours",
            "negative_outcomes_90d",
            "escalation_history",
            "customer_effort_msgs",
            "repeated_issues",
            "unresolved_known_issues",
            "incident_exposure",
        ] {
            let m = metric(key);
            assert!(m["label"].is_string(), "{key} label");
            assert!(m["display"].is_string(), "{key} display");
            assert!(m["definition"].is_string(), "{key} definition");
            assert!(m["evidence_conversation_ids"].is_array(), "{key} evidence");
            assert!(m["completeness"].is_string(), "{key} completeness");
        }
        assert_eq!(metric("open_conversations")["value"], json!(0));
        assert_eq!(metric("support_volume_total")["value"], json!(0));
        assert_eq!(
            metric("first_response_avg_hours")["display"],
            json!("unknown")
        );
        assert_eq!(h["flags"].as_array().unwrap().len(), 0);
        assert_eq!(h["incident_exposure"].as_array().unwrap().len(), 0);
        // Unknown id -> None (404 at the route).
        assert!(customer_support_health(&conn, cid + 999).unwrap().is_none());
    }

    #[test]
    fn support_health_flags_fire_over_real_data() {
        let conn = fresh_db();
        let cid = create_customer(
            &conn,
            &NewCustomer {
                first_name: Some("Carol".into()),
                last_name: Some("Client".into()),
                email: None,
                organization: None,
                job_title: None,
                phone: None,
            },
        )
        .unwrap();
        // One active conversation waiting 10 days + a second active one (plus
        // a third closed conversation for the repeated-issue cluster below).
        for (remote, status) in [(1i64, "active"), (2, "active"), (3, "closed")] {
            conn.execute(
                "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id,
                                            created_at, updated_at, customer_waiting_since)
                 VALUES (?1, ?1, 'Q', ?2, 1, ?3,
                         datetime('now', '-12 days'), datetime('now', '-12 days'), datetime('now', '-10 days'))",
                params![remote, status, cid],
            )
            .unwrap();
        }
        // Two not-good ratings in the last 90 days (negative_outcomes flag).
        for i in 0..2i64 {
            conn.execute(
                "INSERT INTO ratings (remote_id, conversation_id, rating, customer_local_id, remote_created_at)
                 VALUES (?1, 1, 'not-good', ?2, datetime('now', '-5 days'))",
                params![900 + i, cid],
            )
            .unwrap();
        }
        // The 'escalated' tag on the two active conversations
        // (escalation_history flag).
        conn.execute(
            "INSERT INTO tags (id, remote_id, name) VALUES (1, 701, 'escalated')",
            [],
        )
        .unwrap();
        for conv in [1i64, 2] {
            conn.execute(
                "INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (?1, 1)",
                params![conv],
            )
            .unwrap();
        }
        // A cluster with 3 member conversations (repeated_issue flag) and an
        // open known issue linked to one open conversation (unresolved flag).
        conn.execute(
            "INSERT INTO issue_clusters (id, name, conversation_count) VALUES (1, 'API sync failures', 3)",
            [],
        )
        .unwrap();
        for conv in [1i64, 2, 3] {
            conn.execute(
                "INSERT INTO issue_cluster_conversations (cluster_id, conversation_id) VALUES (1, ?1)",
                params![conv],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO known_issues (id, name, status) VALUES (1, 'Login broken', 'investigating')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO known_issue_links (known_issue_id, conversation_id) VALUES (1, 1)",
            [],
        )
        .unwrap();
        // An active incident linked to conversation 1 (exposure + flag).
        conn.execute(
            "INSERT INTO incidents (id, code, title, status, severity) VALUES (1, 'INC-1', 'Login outage', 'investigating', 'sev2')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO incident_conversations (incident_id, conversation_id) VALUES (1, 1)",
            [],
        )
        .unwrap();

        let h = customer_support_health(&conn, cid).unwrap().unwrap();
        let metric = |key: &str| {
            h["metrics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["key"] == json!(key))
                .unwrap()
                .clone()
        };
        assert_eq!(metric("open_conversations")["value"], json!(2));
        assert_eq!(metric("support_volume_total")["value"], json!(3));
        assert_eq!(metric("waiting_count")["value"], json!(2));
        let waiting_avg = metric("waiting_avg_days");
        assert!(waiting_avg["display"].as_str().unwrap().ends_with(" days"));
        assert_eq!(metric("negative_outcomes_90d")["value"], json!(2));
        assert_eq!(metric("escalation_history")["value"], json!(2));
        assert_eq!(metric("repeated_issues")["value"], json!(3));
        assert!(metric("repeated_issues")["display"]
            .as_str()
            .unwrap()
            .contains("API sync failures"));
        assert_eq!(metric("unresolved_known_issues")["value"], json!(1));
        assert_eq!(metric("incident_exposure")["value"], json!(1));

        // Every metric's evidence is bounded and non-empty where data exists.
        for m in h["metrics"].as_array().unwrap() {
            let ev = m["evidence_conversation_ids"].as_array().unwrap();
            assert!(ev.len() <= 10, "evidence bounded: {m:?}");
        }
        assert!(!metric("open_conversations")["evidence_conversation_ids"]
            .as_array()
            .unwrap()
            .is_empty());

        // The flags that fire over this world, each with evidence links.
        let flag_keys: Vec<String> = h["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["key"].as_str().unwrap().to_string())
            .collect();
        for expected in [
            "waiting_long",
            "negative_outcomes",
            "escalation_history",
            "repeated_issue",
            "unresolved_issues",
            "incident_1",
        ] {
            assert!(
                flag_keys.iter().any(|k| k == expected),
                "flag {expected} missing: {flag_keys:?}"
            );
        }
        let waiting_flag = h["flags"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == json!("waiting_long"))
            .unwrap();
        assert_eq!(waiting_flag["severity"], json!("critical"));
        assert!(waiting_flag["detail"]
            .as_str()
            .unwrap()
            .contains("calendar days"));
        let incident_flag = h["flags"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == json!("incident_1"))
            .unwrap();
        assert_eq!(
            incident_flag["severity"],
            json!("warning"),
            "sev2 -> warning"
        );
        assert!(incident_flag["label"].as_str().unwrap().contains("INC-1"));

        // incident_exposure rows carry the reference shape.
        let exposure = &h["incident_exposure"][0];
        assert_eq!(exposure["incident_id"], json!(1));
        assert_eq!(exposure["code"], json!("INC-1"));
        assert_eq!(exposure["title"], json!("Login outage"));
        assert_eq!(exposure["severity"], json!("sev2"));
        assert_eq!(exposure["conversations"], json!(1));
    }

    #[test]
    fn organization_support_health_over_members() {
        let conn = fresh_db();
        let org_id = create_organization(&conn, "Acme", &[]).unwrap();
        let cid = create_customer(
            &conn,
            &NewCustomer {
                first_name: Some("Ada".into()),
                last_name: Some("Member".into()),
                email: None,
                organization: Some("Acme".into()),
                job_title: None,
                phone: None,
            },
        )
        .unwrap();
        // Link the member row to the organization properly.
        conn.execute(
            "UPDATE customers SET organization_id = ?1 WHERE id = ?2",
            params![org_id, cid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id, created_at)
             VALUES (1, 1, 'Q', 'closed', 1, ?1, datetime('now', '-2 days'))",
            params![cid],
        )
        .unwrap();
        let h = organization_support_health(&conn, org_id).unwrap().unwrap();
        assert_eq!(h["subject_kind"], "organization");
        assert_eq!(h["subject_label"], "Acme");
        assert_eq!(
            h["metrics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["key"] == json!("support_volume_total"))
                .unwrap()["value"],
            json!(1),
            "member conversations roll up"
        );
        // Unknown org -> None.
        assert!(organization_support_health(&conn, org_id + 999)
            .unwrap()
            .is_none());
    }

    #[test]
    fn timeline_rebuild_runs_and_resolves_links() {
        let conn = fresh_db();
        // A customer created with an org name that exists.
        let org_id = create_organization(&conn, "Acme", &[]).unwrap();
        let cid = create_customer(
            &conn,
            &NewCustomer {
                first_name: Some("Ada".into()),
                last_name: None,
                email: None,
                organization: Some("Acme".into()),
                job_title: None,
                phone: None,
            },
        )
        .unwrap();
        // One conversation so the sweep has something to derive.
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id, created_at, updated_at)
             VALUES (1, 1, 'Help', 'closed', 1, ?1, '2026-10-01T10:00:00.000Z', '2026-10-01T11:00:00.000Z')",
            params![cid],
        )
        .unwrap();
        let created = timeline_rebuild(&conn).unwrap();
        assert!(created > 0, "the sweep should derive at least one event");
        // The org timeline now has member events.
        let (org_events, total) =
            crate::customer_events::list_for_organization(&conn, org_id, None, 50, 0).unwrap();
        assert_eq!(total, org_events.len() as i64);
        assert!(!org_events.is_empty());
    }
}
