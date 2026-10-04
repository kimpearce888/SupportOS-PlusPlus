//! Issue impact intelligence — the port of the reference
//! `src/server/issues/impact.ts` (plan Phase 19).
//!
//! ONE shared implementation for BOTH incidents (conversation links in
//! `incident_conversations`) and known issues (links in
//! `known_issue_links` — the reference's `known_issue_conversations`):
//! the metric shape is identical, only the link table differs, so there
//! is no parallel implementation to drift.
//!
//! Invariants carried over verbatim:
//! - Affected customers are COUNT(DISTINCT customer) — a ticket count is
//!   NEVER silently used as a customer count (the plan's explicit rule).
//! - Products are read from the current local AI attribute layer
//!   (`ai_attributes`, source-labeled); when no attributes exist the list
//!   is empty and the note says why — nothing is invented.
//! - Release correlation is a TEMPORAL ASSOCIATION ONLY: conversations
//!   starting within 7 days after a release date. The note says so in
//!   plain words; the service never claims causation.
//!
//! Port column mapping (reference → port): `customer_local_id` →
//! `customer_id`, `mailbox_local_id` → `mailbox_id`, `remote_created_at`
//! → `created_at`, `cu.organization_id` (FK) → `cu.organization` (TEXT,
//! grouped by name), `ct.tag_local_id` → `ct.tag_id`.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// The fixed explanatory note every impact payload carries (reference
/// impact.ts:159 — the honest-labeling contract).
const IMPACT_NOTE: &str = "Customer and organization counts are distinct entities, never ticket counts. Products come from the local AI attribute layer (empty until conversations are analyzed). Release entries describe temporal associations only.";

/// Impact for an incident, or `None` when the incident does not exist.
#[must_use]
pub fn for_incident(conn: &Connection, incident_id: i64) -> Option<Value> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM incidents WHERE id = ?1",
            params![incident_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        return None;
    }
    Some(compute(
        conn,
        incident_id,
        "incident",
        "incident_conversations",
        "incident_id",
    ))
}

/// Impact for a known issue, or `None` when the known issue does not exist.
#[must_use]
pub fn for_known_issue(conn: &Connection, known_issue_id: i64) -> Option<Value> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM known_issues WHERE id = ?1",
            params![known_issue_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        return None;
    }
    Some(compute(
        conn,
        known_issue_id,
        "known_issue",
        "known_issue_links",
        "known_issue_id",
    ))
}

/// The shared compute (reference `IssueImpactService.compute`). `link_table`
/// and `link_column` are hard-coded constants from this file's call sites —
/// never user input. Every value below is a bound parameter; the `?1`
/// occurrences (the link-table subquery appears nested in several
/// statements) all bind the same subject id.
fn compute(
    conn: &Connection,
    subject_id: i64,
    subject_kind: &str,
    link_table: &str,
    link_column: &str,
) -> Value {
    let conv_ids_sql = format!(
        "SELECT l.conversation_id AS id FROM {link_table} l
          JOIN conversations c ON c.id = l.conversation_id
          WHERE l.{link_column} = ?1 AND c.deleted_at IS NULL"
    );
    let base =
        format!("FROM conversations c WHERE c.id IN ({conv_ids_sql}) AND c.deleted_at IS NULL");

    // Base counts. The organization subquery nests the link-table
    // subquery — the same `?1` binds both occurrences.
    let counts: (
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
    ) = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) AS conversations,
                   COUNT(DISTINCT c.customer_id) AS customers,
                   (SELECT COUNT(DISTINCT cu.organization) FROM conversations c2
                      JOIN customers cu ON cu.id = c2.customer_id
                      WHERE c2.id IN ({conv_ids_sql}) AND c2.deleted_at IS NULL
                        AND cu.organization IS NOT NULL AND TRIM(cu.organization) != '') AS organizations,
                   MIN(c.created_at) AS first_seen,
                   MAX(c.created_at) AS last_seen,
                   SUM(CASE WHEN c.status = 'active' THEN 1 ELSE 0 END) AS open_count,
                   SUM(CASE WHEN c.status = 'closed' THEN 1 ELSE 0 END) AS closed_count,
                   SUM(CASE WHEN c.customer_waiting_since IS NOT NULL AND c.status = 'active' THEN 1 ELSE 0 END) AS waiting_count
                 {base}"
            ),
            params![subject_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                ))
            },
        )
        .unwrap_or((0, 0, 0, None, None, 0, 0, 0));
    let (
        conversations,
        customers,
        organizations,
        first_seen,
        last_seen,
        open_count,
        closed_count,
        waiting_count,
    ) = counts;

    // Growth: conversations started in the last 7 days vs the previous 7.
    let recent = linked_since(conn, subject_id, &base, "-7 days");
    let previous = linked_between(conn, subject_id, &base, "-14 days", "-7 days");
    let recent14 = linked_since(conn, subject_id, &base, "-14 days");
    let prior14 = linked_between(conn, subject_id, &base, "-28 days", "-14 days");
    let (ratio, direction) = if previous > 0 {
        let r = recent as f64 / previous as f64;
        let d = if r >= 1.3 {
            "rising"
        } else if r <= 0.7 {
            "falling"
        } else {
            "flat"
        };
        (json!(r), d)
    } else if recent > 0 {
        (Value::Null, "rising")
    } else {
        (json!(0.0), "unknown")
    };

    // Trend: new (first seen within 7 days) / rising / falling / stable.
    let trend = if conversations == 0 {
        "unknown"
    } else if first_seen
        .as_deref()
        .map(|fs| {
            conn.query_row(
                "SELECT julianday(?1) >= julianday('now', '-7 days')",
                params![fs],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v == 1)
            .unwrap_or(false)
        })
        .unwrap_or(false)
    {
        "new"
    } else if prior14 == 0 && recent14 >= 1 {
        "rising"
    } else if prior14 > 0 && (recent14 as f64) < (prior14 as f64) * 0.7 {
        "falling"
    } else {
        "stable"
    };

    let inboxes = collect_pairs(
        conn,
        &format!(
            "SELECT (SELECT m.name FROM mailboxes m WHERE m.id = c.mailbox_id) AS mailbox,
                    COUNT(*) AS conversations
             {base} GROUP BY c.mailbox_id ORDER BY conversations DESC LIMIT 10"
        ),
        subject_id,
        "mailbox",
        "conversations",
    );

    let tags = collect_pairs(
        conn,
        &format!(
            "SELECT t.name AS tag, COUNT(*) AS conversations
             FROM conversation_tags ct
               JOIN tags t ON t.id = ct.tag_id
             WHERE ct.conversation_id IN ({conv_ids_sql})
             GROUP BY t.name ORDER BY conversations DESC LIMIT 10"
        ),
        subject_id,
        "tag",
        "conversations",
    );

    // Products from the local AI attribute layer (labeled, evidence-backed;
    // empty when nothing has been analyzed — the honest state).
    let products = collect_pairs(
        conn,
        &format!(
            "SELECT aa.value AS product, COUNT(DISTINCT aa.conversation_id) AS conversations
             FROM ai_attributes aa
             WHERE aa.attribute = 'product' AND aa.superseded_at IS NULL AND aa.source = 'ai'
               AND aa.conversation_id IN ({conv_ids_sql}) AND aa.confidence IN ('high', 'medium')
             GROUP BY aa.value ORDER BY conversations DESC LIMIT 10"
        ),
        subject_id,
        "product",
        "conversations",
    );

    // Release correlation (incidents only): conversations starting within
    // 7 days after a recorded release date. Temporal association, nothing
    // more.
    let mut release_candidates: Vec<Value> = Vec::new();
    if subject_kind == "incident" {
        let releases: Vec<(i64, String, String)> = conn
            .prepare(
                "SELECT id, version_label, released_at FROM incident_releases
                 WHERE incident_id = ?1 AND released_at IS NOT NULL",
            )
            .and_then(|mut stmt| {
                let rows = stmt.query_map(params![subject_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default();
        for (_, label, released_at) in releases {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) {base}
                         AND julianday(c.created_at) >= julianday(?2)
                         AND julianday(c.created_at) < julianday(?2, '+7 days')"
                    ),
                    params![subject_id, released_at],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if n > 0 {
                release_candidates.push(json!({
                    "version_label": label,
                    "released_at": released_at,
                    "conversations_within_7d": n,
                    "note": format!("{n} linked conversations started within 7 days after this release date - a temporal association, not a causal claim."),
                }));
            }
        }
    }

    json!({
        "subject_kind": subject_kind,
        "subject_id": subject_id,
        "affected_conversations": conversations,
        "affected_customers": customers,
        "affected_organizations": organizations,
        "first_seen_at": first_seen,
        "last_seen_at": last_seen,
        "growth_rate_7d": {
            "recent": recent,
            "previous": previous,
            "ratio": ratio,
            "direction": direction,
        },
        "trend": trend,
        "affected_inboxes": inboxes,
        "top_tags": tags,
        "products": products,
        "open_closed_distribution": { "open": open_count, "closed": closed_count },
        "customer_waiting_count": waiting_count,
        "release_correlation_candidates": release_candidates,
        "note": IMPACT_NOTE,
    })
}

/// COUNT of linked conversations with `julianday(created_at) >= julianday('now', since)`.
fn linked_since(conn: &Connection, subject_id: i64, base: &str, since: &str) -> i64 {
    conn.query_row(
        &format!(
            "SELECT COUNT(*) AS n {base} AND julianday(c.created_at) >= julianday('now', '{since}')"
        ),
        params![subject_id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// COUNT of linked conversations with created_at in [from, to) relative to now.
fn linked_between(conn: &Connection, subject_id: i64, base: &str, from: &str, to: &str) -> i64 {
    conn.query_row(
        &format!(
            "SELECT COUNT(*) AS n {base}
             AND julianday(c.created_at) >= julianday('now', '{from}')
             AND julianday(c.created_at) < julianday('now', '{to}')"
        ),
        params![subject_id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Run a `{key} AS ..., COUNT(*) AS count_key` breakdown query and collect
/// `[{key: value|null, count_key: n}, ...]` rows.
fn collect_pairs(
    conn: &Connection,
    sql: &str,
    subject_id: i64,
    key: &str,
    count_key: &str,
) -> Vec<Value> {
    conn.prepare(sql)
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![subject_id], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .map(|rows| {
            rows.into_iter()
                .map(|(k, n)| json!({ key: k, count_key: n }))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection as Conn;

    fn fresh_db() -> Conn {
        // The real production boot chain — the impact SQL must work against
        // exactly what `bootstrap::apply_all` produces.
        let mut conn = Conn::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn iso_days_ago(days: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::hours(days * 24 + 12))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_conversation(
        conn: &Conn,
        id: i64,
        number: i64,
        customer: i64,
        mailbox: i64,
        status: &str,
        days_ago: i64,
        waiting: bool,
    ) {
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at, customer_waiting_since)
             VALUES (?1, ?1, ?2, 's', ?3, ?4, ?5, ?6, ?7)",
            params![id, number, status, mailbox, customer, iso_days_ago(days_ago), if waiting { Some(iso_days_ago(1)) } else { None }],
        )
        .unwrap();
    }

    fn seed_customer(conn: &Conn, id: i64, org: Option<&str>) {
        conn.execute(
            "INSERT INTO customers (id, remote_id, first_name, last_name, email, organization)
             VALUES (?1, ?1, 'First', 'Last', ?2, ?3)",
            params![id, format!("c{id}@example.com"), org],
        )
        .unwrap();
    }

    fn make_incident(conn: &Conn, conv_ids: &[i64]) -> i64 {
        let incident = crate::intelligence_features::ManualIncident {
            title: "Impact test".into(),
            status: "investigating".into(),
            severity: "sev2".into(),
            owner_user_local_id: None,
            product: None,
            feature: None,
            description: None,
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
    fn missing_subjects_return_none() {
        let conn = fresh_db();
        assert!(for_incident(&conn, 999).is_none());
        assert!(for_known_issue(&conn, 999).is_none());
    }

    #[test]
    fn counts_are_distinct_entities_not_ticket_counts() {
        let conn = fresh_db();
        seed_customer(&conn, 1, Some("Acme"));
        seed_customer(&conn, 2, Some("Acme"));
        seed_customer(&conn, 3, Some("Globex"));
        // Customer 1 files 3 tickets, customers 2 and 3 file one each.
        seed_conversation(&conn, 1, 101, 1, 10, "active", 5, true);
        seed_conversation(&conn, 2, 102, 1, 10, "closed", 10, false);
        seed_conversation(&conn, 3, 103, 1, 20, "active", 3, false);
        seed_conversation(&conn, 4, 104, 2, 20, "closed", 8, false);
        seed_conversation(&conn, 5, 105, 3, 20, "active", 1, false);
        let id = make_incident(&conn, &[1, 2, 3, 4, 5]);

        let impact = for_incident(&conn, id).unwrap();
        assert_eq!(impact["subject_kind"], "incident");
        assert_eq!(impact["affected_conversations"], 5);
        assert_eq!(impact["affected_customers"], 3); // never 5
        assert_eq!(impact["affected_organizations"], 2); // Acme + Globex
        assert_eq!(impact["open_closed_distribution"]["open"], 3);
        assert_eq!(impact["open_closed_distribution"]["closed"], 2);
        assert_eq!(impact["customer_waiting_count"], 1);
        assert!(impact["first_seen_at"].as_str().is_some());
        assert!(impact["last_seen_at"].as_str().is_some());
    }

    #[test]
    fn deleted_conversations_are_excluded() {
        let conn = fresh_db();
        seed_customer(&conn, 1, None);
        seed_conversation(&conn, 1, 101, 1, 10, "active", 5, false);
        seed_conversation(&conn, 2, 102, 1, 10, "active", 5, false);
        conn.execute(
            "UPDATE conversations SET deleted_at = ?1 WHERE id = 2",
            params![iso_days_ago(1)],
        )
        .unwrap();
        let id = make_incident(&conn, &[1, 2]);
        let impact = for_incident(&conn, id).unwrap();
        assert_eq!(impact["affected_conversations"], 1);
    }

    #[test]
    fn growth_direction_uses_ratio_thresholds() {
        let conn = fresh_db();
        seed_customer(&conn, 1, None);
        // 1 in the previous window, 3 in the recent one: ratio 3.0 → rising.
        seed_conversation(&conn, 1, 101, 1, 10, "closed", 10, false);
        seed_conversation(&conn, 2, 102, 1, 10, "active", 2, false);
        seed_conversation(&conn, 3, 103, 1, 10, "active", 1, false);
        seed_conversation(&conn, 4, 104, 1, 10, "active", 0, false);
        let id = make_incident(&conn, &[1, 2, 3, 4]);
        let impact = for_incident(&conn, id).unwrap();
        assert_eq!(impact["growth_rate_7d"]["recent"], 3);
        assert_eq!(impact["growth_rate_7d"]["previous"], 1);
        assert_eq!(impact["growth_rate_7d"]["direction"], "rising");
        // First seen 10 days ago, prior window empty but recent >= 1 → trend
        // rising (not new).
        assert_eq!(impact["trend"], "rising");
    }

    #[test]
    fn recent_first_seen_is_trend_new() {
        let conn = fresh_db();
        seed_customer(&conn, 1, None);
        seed_conversation(&conn, 1, 101, 1, 10, "active", 2, false);
        let id = make_incident(&conn, &[1]);
        let impact = for_incident(&conn, id).unwrap();
        assert_eq!(impact["trend"], "new");
        // previous == 0 with recent > 0 → ratio null, direction rising.
        assert!(impact["growth_rate_7d"]["ratio"].is_null());
        assert_eq!(impact["growth_rate_7d"]["direction"], "rising");
    }

    #[test]
    fn empty_incident_is_unknown_everywhere() {
        let conn = fresh_db();
        let id = make_incident(&conn, &[]);
        let impact = for_incident(&conn, id).unwrap();
        assert_eq!(impact["affected_conversations"], 0);
        assert_eq!(impact["trend"], "unknown");
        assert_eq!(impact["growth_rate_7d"]["direction"], "unknown");
        assert_eq!(impact["growth_rate_7d"]["ratio"], 0.0);
        assert!(impact["first_seen_at"].is_null());
        assert_eq!(impact["affected_inboxes"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn breakdowns_cover_inboxes_tags_and_products() {
        let conn = fresh_db();
        seed_customer(&conn, 1, None);
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (10, 10, 'Support'), (20, 20, 'Billing')",
            [],
        )
        .unwrap();
        seed_conversation(&conn, 1, 101, 1, 10, "active", 5, false);
        seed_conversation(&conn, 2, 102, 1, 10, "active", 5, false);
        seed_conversation(&conn, 3, 103, 1, 20, "active", 5, false);
        conn.execute(
            "INSERT INTO tags (id, remote_id, name) VALUES (7, 7, 'bug')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (1, 7), (2, 7), (3, 7)",
            [],
        )
        .unwrap();
        // One current high-confidence AI product attribute on conv 1, one
        // superseded (must be ignored), one low-confidence (must be ignored).
        conn.execute(
            "INSERT INTO ai_attributes (conversation_id, attribute, value, value_type, confidence, source, schema_version, computed_at)
             VALUES
               (1, 'product', 'Billing', 'text', 'high', 'ai', 'v1', datetime('now')),
               (2, 'product', 'Billing-old', 'text', 'high', 'ai', 'v1', datetime('now', '-2 days')),
               (3, 'product', 'Search', 'text', 'low', 'ai', 'v1', datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE ai_attributes SET superseded_at = datetime('now', '-1 days')
             WHERE conversation_id = 2 AND value = 'Billing-old'",
            [],
        )
        .unwrap();
        let id = make_incident(&conn, &[1, 2, 3]);

        let impact = for_incident(&conn, id).unwrap();
        let inboxes = impact["affected_inboxes"].as_array().unwrap();
        assert_eq!(inboxes.len(), 2);
        assert_eq!(inboxes[0]["mailbox"], "Support");
        assert_eq!(inboxes[0]["conversations"], 2);
        assert_eq!(inboxes[1]["mailbox"], "Billing");

        let tags = impact["top_tags"].as_array().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0]["tag"], "bug");
        assert_eq!(tags[0]["conversations"], 3);

        let products = impact["products"].as_array().unwrap();
        assert_eq!(products.len(), 1);
        assert_eq!(products[0]["product"], "Billing");
        assert_eq!(products[0]["conversations"], 1);
    }

    #[test]
    fn release_correlation_counts_conversations_within_7d() {
        let conn = fresh_db();
        seed_customer(&conn, 1, None);
        // 2 conversations started within 7 days after the release, 1 before.
        seed_conversation(&conn, 1, 101, 1, 10, "closed", 30, false);
        seed_conversation(&conn, 2, 102, 1, 10, "closed", 3, false);
        seed_conversation(&conn, 3, 103, 1, 10, "closed", 1, false);
        let id = make_incident(&conn, &[1, 2, 3]);
        let release = iso_days_ago(5);
        crate::intelligence_features::add_incident_release(
            &conn,
            id,
            "v4.12.0",
            Some(&release),
            None,
        )
        .unwrap();

        let impact = for_incident(&conn, id).unwrap();
        let candidates = impact["release_correlation_candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["version_label"], "v4.12.0");
        assert_eq!(candidates[0]["conversations_within_7d"], 2);
        assert!(candidates[0]["note"]
            .as_str()
            .unwrap()
            .contains("temporal association"));
    }

    #[test]
    fn known_issue_impact_uses_known_issue_links() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO known_issues (id, name) VALUES (1, 'Broken export')",
            [],
        )
        .unwrap();
        seed_customer(&conn, 1, Some("Acme"));
        seed_customer(&conn, 2, Some("Initech"));
        seed_conversation(&conn, 1, 101, 1, 10, "active", 4, false);
        seed_conversation(&conn, 2, 102, 2, 10, "active", 2, false);
        conn.execute(
            "INSERT INTO known_issue_links (known_issue_id, conversation_id) VALUES (1, 1), (1, 2)",
            [],
        )
        .unwrap();

        let impact = for_known_issue(&conn, 1).unwrap();
        assert_eq!(impact["subject_kind"], "known_issue");
        assert_eq!(impact["subject_id"], 1);
        assert_eq!(impact["affected_conversations"], 2);
        assert_eq!(impact["affected_customers"], 2);
        assert_eq!(impact["affected_organizations"], 2);
        // Known issues never carry release correlation candidates.
        assert_eq!(
            impact["release_correlation_candidates"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
    }
}
