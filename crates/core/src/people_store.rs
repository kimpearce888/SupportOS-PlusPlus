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

/// Deterministic support-health for one customer over their real
/// conversations. Rules (documented, no ML):
/// - `unknown` — no conversations at all.
/// - `at_risk` — 3+ active conversations, OR resolution rate below 50%
///   with at least 5 closed conversations, OR the newest customer message
///   is older than 30 days while a conversation is still active (stalled).
/// - `healthy` — everything else.
///
/// The payload keeps the reference `{health}` envelope and adds the numbers
/// it was computed from so the UI can render them without a second call.
pub fn customer_support_health(conn: &Connection, customer_id: i64) -> Result<Option<Value>> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM customers WHERE id = ?1 AND deleted_at IS NULL",
            params![customer_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        return Ok(None);
    }
    let (total, active, closed): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status IN ('closed','resolved') THEN 1 ELSE 0 END), 0)
         FROM conversations
         WHERE customer_id = ?1 AND deleted_at IS NULL",
        params![customer_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let last_activity: Option<String> = conn
        .query_row(
            "SELECT MAX(COALESCE(updated_at, local_created_at)) FROM conversations
             WHERE customer_id = ?1 AND deleted_at IS NULL",
            params![customer_id],
            |r| r.get(0),
        )
        .unwrap_or(None);

    let resolution_rate = if (closed + active).max(1) > 0 {
        closed as f64 / (closed + active).max(1) as f64
    } else {
        0.0
    };
    let health = if total == 0 {
        "unknown"
    } else if active >= 3 || (closed >= 5 && resolution_rate < 0.5) {
        "at_risk"
    } else if let (Some(last), true) = (last_activity.as_deref(), active > 0) {
        if stalled(last, 30) {
            "at_risk"
        } else {
            "healthy"
        }
    } else {
        "healthy"
    };

    Ok(Some(json!({
        "health": health,
        "conversation_count": total,
        "active_count": active,
        "closed_count": closed,
        "resolution_rate": if total > 0 { closed as f64 / total as f64 } else { 0.0 },
        "last_activity_at": last_activity,
    })))
}

/// Deterministic support-health for one organization: the same rules applied
/// to the union of all member conversations.
pub fn organization_support_health(conn: &Connection, org_id: i64) -> Result<Option<Value>> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM organizations WHERE id = ?1 AND deleted_at IS NULL",
            params![org_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        return Ok(None);
    }
    let (total, active, closed): (i64, i64, i64) = conn.query_row(
        &format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN c.status = 'active' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN c.status IN ('closed','resolved') THEN 1 ELSE 0 END), 0)
             FROM conversations c
             JOIN customers cu ON cu.id = c.customer_id
             WHERE {ORG_MEMBERS_SQL} AND c.deleted_at IS NULL AND cu.deleted_at IS NULL"
        ),
        params![org_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let last_activity: Option<String> = conn
        .query_row(
            &format!(
                "SELECT MAX(COALESCE(c.updated_at, c.local_created_at)) FROM conversations c
                 JOIN customers cu ON cu.id = c.customer_id
                 WHERE {ORG_MEMBERS_SQL} AND c.deleted_at IS NULL AND cu.deleted_at IS NULL"
            ),
            params![org_id],
            |r| r.get(0),
        )
        .unwrap_or(None);

    let resolution_rate = if (closed + active).max(1) > 0 {
        closed as f64 / (closed + active).max(1) as f64
    } else {
        0.0
    };
    let health = if total == 0 {
        "unknown"
    } else if active >= 5 || (closed >= 10 && resolution_rate < 0.5) {
        "at_risk"
    } else if let (Some(last), true) = (last_activity.as_deref(), active > 0) {
        if stalled(last, 30) {
            "at_risk"
        } else {
            "healthy"
        }
    } else {
        "healthy"
    };

    Ok(Some(json!({
        "health": health,
        "conversation_count": total,
        "active_count": active,
        "closed_count": closed,
        "resolution_rate": if total > 0 { closed as f64 / total as f64 } else { 0.0 },
        "last_activity_at": last_activity,
    })))
}

/// A conversation is stalled when its newest activity is older than `days`.
/// Timestamps are ISO-ish (`YYYY-MM-DDTHH:MM:SS...` or SQLite
/// `datetime('now')` output) — compare lexically after trimming to the first
/// 19 chars, which is correct for both formats, and treat an unparseable
/// value as NOT stalled (never punish weird data with a false alarm).
fn stalled(last_activity: &str, days: i64) -> bool {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let last = last_activity.trim();
    if last.len() < 19 || cutoff.len() < 19 {
        return false;
    }
    last.as_bytes()[..19] < cutoff.as_bytes()[..19]
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
/// FK backfill, then report both counts. Returns (events_written,
/// org_links_resolved).
pub fn timeline_rebuild(conn: &Connection) -> Result<(usize, usize)> {
    let events = crate::customer_events::rebuild(conn)?;
    let links = conn.execute(
        "UPDATE customers SET organization_id = (
            SELECT o.id FROM organizations o
             WHERE o.name = customers.organization AND o.deleted_at IS NULL LIMIT 1)
         WHERE organization_id IS NULL AND organization IS NOT NULL AND organization != ''",
        [],
    )?;
    let _ = crate::audit::audit(
        conn,
        &crate::audit::AuditEntry::user("timeline_rebuilt").with_after_state(json!({
            "events_written": events,
            "org_links_resolved": links,
        })),
    );
    Ok((events, links))
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
    fn support_health_is_deterministic() {
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
        // No conversations → unknown.
        let h = customer_support_health(&conn, cid).unwrap().unwrap();
        assert_eq!(h["health"], "unknown");
        assert!(customer_support_health(&conn, cid + 999).unwrap().is_none());

        // One closed conversation → healthy.
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id, updated_at)
             VALUES (1, 1, 'Q', 'closed', 1, ?1, datetime('now'))",
            params![cid],
        )
        .unwrap();
        let h = customer_support_health(&conn, cid).unwrap().unwrap();
        assert_eq!(h["health"], "healthy");
        assert_eq!(h["conversation_count"], json!(1));

        // Three active conversations → at_risk.
        for i in 2..=4i64 {
            conn.execute(
                "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id, updated_at)
                 VALUES (?1, ?1, 'Q', 'active', 1, ?2, datetime('now'))",
                params![i, cid],
            )
            .unwrap();
        }
        let h = customer_support_health(&conn, cid).unwrap().unwrap();
        assert_eq!(h["health"], "at_risk");
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
        let (events, links) = timeline_rebuild(&conn).unwrap();
        assert!(events > 0, "the sweep should derive at least one event");
        assert!(links <= events, "links are a subset of events");
        // The org timeline now has member events.
        let (org_events, total) =
            crate::customer_events::list_for_organization(&conn, org_id, None, 50, 0).unwrap();
        assert_eq!(total, org_events.len() as i64);
        assert!(!org_events.is_empty());
    }
}
