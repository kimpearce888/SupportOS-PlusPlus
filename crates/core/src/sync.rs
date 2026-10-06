//! Sync mirror write-layer — cursor/checkpoint helpers + the Help Scout
//! upsert helpers the SyncEngine (sync_engine.rs) and the worker job
//! executor share. The coordinator itself (initial/incremental/reconcile)
//! lives in `sync_engine.rs`; this module owns the SQL that lands rows.

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::helpscout::{
    HsConversation, HsCustomer, HsCustomerAddress, HsCustomerEmail, HsCustomerPhone,
    HsCustomerPropertyValue, HsCustomerSocialProfile, HsCustomerWebsite, HsMailbox, HsTag, HsTeam,
    HsUser,
};

// ---------------------------------------------------------------------------
// Write helpers (upsert data into SQLite)
// ---------------------------------------------------------------------------

/// Upsert a mailbox into the `mailboxes` table.
pub fn upsert_mailbox(conn: &Connection, m: &HsMailbox) -> Result<()> {
    conn.execute(
        "INSERT INTO mailboxes (remote_id, name, slug, email, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(remote_id) DO UPDATE SET
            name = excluded.name,
            slug = excluded.slug,
            email = excluded.email,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at",
        params![
            m.remote_id,
            m.name,
            m.slug,
            m.email,
            m.created_at,
            m.updated_at
        ],
    )?;
    Ok(())
}

/// Upsert a user into the `users` table.
pub fn upsert_user(conn: &Connection, u: &HsUser) -> Result<()> {
    conn.execute(
        "INSERT INTO users (remote_id, first_name, last_name, email, role, user_type, timezone,
            photo_url, initials, mention, job_title, phone, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(remote_id) DO UPDATE SET
            first_name = excluded.first_name,
            last_name = excluded.last_name,
            email = excluded.email,
            role = excluded.role,
            user_type = excluded.user_type,
            timezone = excluded.timezone,
            photo_url = excluded.photo_url,
            initials = excluded.initials,
            mention = excluded.mention,
            job_title = excluded.job_title,
            phone = excluded.phone,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at",
        params![
            u.remote_id,
            u.first_name,
            u.last_name,
            u.email,
            u.role,
            u.user_type,
            u.timezone,
            u.photo_url,
            u.initials,
            u.mention,
            u.job_title,
            u.phone,
            u.created_at,
            u.updated_at,
        ],
    )?;
    Ok(())
}

/// Upsert a team into the `teams` table + refresh its membership
/// (`team_members`) — the reference `upsertTeam` + `upsertTeamMembers`
/// (referenceRepo.ts:125-143): members are replaced wholesale with the
/// remote list, resolved to LOCAL user ids.
pub fn upsert_team(conn: &Connection, t: &HsTeam) -> Result<()> {
    conn.execute(
        "INSERT INTO teams (remote_id, name)
         VALUES (?1, ?2)
         ON CONFLICT(remote_id) DO UPDATE SET name = excluded.name",
        params![t.remote_id, t.name],
    )?;
    let team_local: Option<i64> = conn
        .query_row(
            "SELECT id FROM teams WHERE remote_id = ?1",
            params![t.remote_id],
            |r| r.get(0),
        )
        .ok();
    if let Some(team_local) = team_local {
        // Older schemas without the team_members mirror (db_breadth's 001
        // shape) simply skip the membership refresh.
        if !crate::sync_schema::table_exists(conn, "team_members").unwrap_or(false) {
            return Ok(());
        }
        conn.execute(
            "DELETE FROM team_members WHERE team_id = ?1",
            params![team_local],
        )?;
        for member_remote in &t.member_user_ids {
            if let Some(user_local) = crate::sync_engine::local_id(conn, "users", *member_remote) {
                conn.execute(
                    "INSERT OR IGNORE INTO team_members (team_id, user_id) VALUES (?1, ?2)",
                    params![team_local, user_local],
                )?;
            }
        }
    }
    Ok(())
}

/// Upsert a tag into the `tags` table.
pub fn upsert_tag(conn: &Connection, t: &HsTag) -> Result<()> {
    conn.execute(
        "INSERT INTO tags (remote_id, name, slug, color, ticket_count, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(remote_id) DO UPDATE SET
            name = excluded.name,
            slug = excluded.slug,
            color = excluded.color,
            ticket_count = excluded.ticket_count,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at",
        params![
            t.remote_id,
            t.name,
            t.slug,
            t.color,
            t.ticket_count,
            t.created_at,
            t.updated_at,
        ],
    )?;
    Ok(())
}

/// Upsert a conversation into the `conversations` table.
///
/// Reference `conversationRepo.upsertConversation` maps every remote id to
/// its LOCAL mirror id before writing (`localIds.mailbox/customer/user`) —
/// `conversations.customer_id` etc. are local foreign keys, and the
/// customer/search/event-timeline queries join on them. The port maps the
/// same way; when a mirror row is missing (the reference writes NULL, the
/// port's columns are NOT NULL) the remote id is kept so the landing never
/// drops a conversation.
///
/// The v1.3.0 channel columns (`type`, `source_type`, `source_via`), the
/// `state`, `thread_count` and `snoozed_until` columns persist with the row
/// (reference migration 001/007 columns) so Beacon chats keep their channel
/// attribution in the mirror.
pub fn upsert_conversation(conn: &Connection, c: &HsConversation) -> Result<()> {
    let mailbox_local = crate::sync_engine::local_id(conn, "mailboxes", c.mailbox_id);
    let customer_local = if c.customer_id > 0 {
        crate::sync_engine::local_id(conn, "customers", c.customer_id)
    } else {
        None
    };
    let assignee_local = c
        .assignee_id
        .filter(|rid| *rid > 0)
        .and_then(|rid| crate::sync_engine::local_id(conn, "users", rid));
    // The stored row BEFORE the upsert — the reference's observation diff
    // base (conversationRepo.ts:168-208). `None` = first ingest (no diff
    // events for a brand-new conversation).
    let previous: Option<Option<i64>> = conn
        .query_row(
            "SELECT assignee_id FROM conversations WHERE remote_id = ?1",
            params![c.remote_id],
            |r| r.get(0),
        )
        .ok();
    conn.execute(
        "INSERT INTO conversations (remote_id, number, subject, preview, status, state, type,
            source_type, source_via, mailbox_id, assignee_id, customer_id, priority, created_at,
            updated_at, closed_at, snoozed_until, thread_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(remote_id) DO UPDATE SET
            number = excluded.number,
            subject = excluded.subject,
            preview = excluded.preview,
            status = excluded.status,
            state = excluded.state,
            type = excluded.type,
            source_type = excluded.source_type,
            source_via = excluded.source_via,
            mailbox_id = excluded.mailbox_id,
            assignee_id = excluded.assignee_id,
            customer_id = excluded.customer_id,
            priority = excluded.priority,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            closed_at = excluded.closed_at,
            snoozed_until = excluded.snoozed_until,
            thread_count = excluded.thread_count",
        params![
            c.remote_id,
            c.number,
            c.subject,
            c.preview,
            c.status,
            c.state,
            c.kind,
            c.source_type,
            c.source_via,
            mailbox_local.unwrap_or(c.mailbox_id),
            assignee_local,
            customer_local.unwrap_or(c.customer_id),
            c.priority,
            c.created_at,
            c.updated_at,
            c.closed_at,
            c.snoozed_until,
            c.thread_count,
        ],
    )?;
    // Mirror the reference's per-conversation tags (conversation_tags join).
    let local: Option<i64> = conn
        .query_row(
            "SELECT id FROM conversations WHERE remote_id = ?1",
            params![c.remote_id],
            |r| r.get(0),
        )
        .ok();
    if let Some(conv_local) = local {
        // Reference updateLocalTags: unknown names get monotonic negative
        // remote ids; case-insensitive lookup; slugified names.
        crate::conversation_ops::write_conversation_tags(conn, conv_local, &c.tags);
    }
    // v1.7.0 conversation change event (sync observation diff): Help Scout
    // exposes no change log, so this honestly records OBSERVATION time with
    // source='sync' — the change happened between the previous observation
    // and this one. The notification sweep turns an assignment observation
    // into a `ticket_assigned` notification (the assignee diff is the only
    // kind the sweep consumes; the reference also records
    // status/team/moved/snooze/tag diffs for the activity timeline).
    if let (Some(prev_assignee), Some(conv_local)) = (previous, local) {
        if prev_assignee != assignee_local {
            let observed_at = crate::activity::now_iso();
            crate::activity::record_full_event(
                conn,
                &crate::activity::FullActivityEvent {
                    base: crate::activity::ActivityEvent {
                        id: None,
                        conversation_id: conv_local,
                        event_type: "assignment_changed".into(),
                        actor_type: "user".into(),
                        actor_id: None,
                        occurred_at: observed_at.clone(),
                        dedup_key: format!("assignment_changed:{}:{observed_at}", c.remote_id),
                    },
                    thread_local_id: None,
                    source: "sync".into(),
                    metadata: Some(
                        serde_json::json!({
                            "previous": prev_assignee,
                            "next": assignee_local,
                            "observed": true
                        })
                        .to_string(),
                    ),
                },
            )?;
        }
    }
    Ok(())
}

/// Upsert a customer into the `customers` table.
pub fn upsert_customer(conn: &Connection, c: &HsCustomer) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO customers (remote_id, first_name, last_name, email, organization, job_title,
            phone, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(remote_id) DO UPDATE SET
            first_name = excluded.first_name,
            last_name = excluded.last_name,
            email = excluded.email,
            organization = excluded.organization,
            job_title = excluded.job_title,
            phone = excluded.phone,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at",
        params![
            c.remote_id,
            c.first_name,
            c.last_name,
            c.email,
            c.organization,
            c.job_title,
            c.phone,
            c.created_at,
            c.updated_at,
        ],
    )?;
    // SY-05 (C8): mirror write fidelity — the contact side tables and the
    // property values are part of the provider's customer object and were
    // dropped before. Replace-set semantics: the mirror reflects the
    // provider's current state (removals propagate).
    let local: i64 = tx.query_row(
        "SELECT id FROM customers WHERE remote_id = ?1",
        params![c.remote_id],
        |r| r.get(0),
    )?;
    let replace_set =
        |delete_sql: &str, insert_sql: &str, rows: Vec<Vec<Box<dyn rusqlite::ToSql>>>| {
            tx.execute(delete_sql, params![local])?;
            for row in rows {
                let mut bind: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(row.len() + 1);
                bind.push(&local);
                for v in &row {
                    bind.push(v.as_ref());
                }
                tx.execute(insert_sql, bind.as_slice())?;
            }
            crate::error::Result::Ok(())
        };
    // Emails / phones / websites / social profiles.
    replace_set(
        "DELETE FROM customer_emails WHERE customer_id = ?1",
        "INSERT INTO customer_emails (customer_id, value, type) VALUES (?1, ?2, ?3)",
        c.emails
            .iter()
            .filter_map(|e| {
                e.value
                    .as_deref()
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| {
                        vec![
                            Box::new(v.trim().to_string()) as Box<dyn rusqlite::ToSql>,
                            Box::new(e.kind.clone()),
                        ]
                    })
            })
            .collect(),
    )?;
    replace_set(
        "DELETE FROM customer_phones WHERE customer_id = ?1",
        "INSERT INTO customer_phones (customer_id, value, type) VALUES (?1, ?2, ?3)",
        c.phones
            .iter()
            .filter_map(|p| {
                p.value
                    .as_deref()
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| {
                        vec![
                            Box::new(v.trim().to_string()) as Box<dyn rusqlite::ToSql>,
                            Box::new(p.kind.clone()),
                        ]
                    })
            })
            .collect(),
    )?;
    replace_set(
        "DELETE FROM customer_websites WHERE customer_id = ?1",
        "INSERT INTO customer_websites (customer_id, value) VALUES (?1, ?2)",
        c.websites
            .iter()
            .filter_map(|w| {
                w.value
                    .as_deref()
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| vec![Box::new(v.trim().to_string()) as Box<dyn rusqlite::ToSql>])
            })
            .collect(),
    )?;
    replace_set(
        "DELETE FROM customer_social_profiles WHERE customer_id = ?1",
        "INSERT INTO customer_social_profiles (customer_id, value, type) VALUES (?1, ?2, ?3)",
        c.social_profiles
            .iter()
            .filter_map(|s| {
                s.value
                    .as_deref()
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| {
                        vec![
                            Box::new(v.trim().to_string()) as Box<dyn rusqlite::ToSql>,
                            Box::new(s.kind.clone()),
                        ]
                    })
            })
            .collect(),
    )?;
    // Postal address (single row per customer).
    tx.execute(
        "DELETE FROM customer_addresses WHERE customer_id = ?1",
        params![local],
    )?;
    if let Some(a) = &c.address {
        let lines = [a.line1.as_deref(), a.line2.as_deref()]
            .into_iter()
            .flatten()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        tx.execute(
            "INSERT INTO customer_addresses (customer_id, lines, city, state, postal_code, country)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                local,
                if lines.is_empty() { None } else { Some(lines) },
                a.city,
                a.state,
                a.postal_code,
                a.country,
            ],
        )?;
    }
    // Property values: resolve the definition by remote id; when the
    // definitions resource has not synced yet, write a stub from the value's
    // own name/key so the property is not silently dropped (the later
    // definitions sync upserts the full definition row).
    for p in &c.properties {
        let value = p.value.as_deref().filter(|v| !v.trim().is_empty());
        if value.is_none() || p.definition_remote_id.is_none() {
            continue;
        }
        let def_remote = p.definition_remote_id.unwrap_or(0);
        let def_local: Option<i64> = tx
            .query_row(
                "SELECT id FROM customer_property_definitions WHERE remote_id = ?1",
                params![def_remote],
                |r| r.get(0),
            )
            .ok();
        let def_local = match def_local {
            Some(id) => id,
            None => {
                let name = p
                    .name
                    .clone()
                    .or_else(|| p.key.clone())
                    .unwrap_or_else(|| format!("Property {def_remote}"));
                tx.execute(
                    "INSERT INTO customer_property_definitions (remote_id, name, slug, type, sort_order, last_synced_at)
                     VALUES (?1, ?2, ?3, 'text', 0, datetime('now'))
                     ON CONFLICT(remote_id) DO UPDATE SET name = excluded.name",
                    params![def_remote, name, p.key],
                )?;
                tx.query_row(
                    "SELECT id FROM customer_property_definitions WHERE remote_id = ?1",
                    params![def_remote],
                    |r| r.get(0),
                )?
            }
        };
        tx.execute(
            "INSERT INTO customer_properties (customer_id, definition_id, value)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(customer_id, definition_id) DO UPDATE SET value = excluded.value",
            params![local, def_local, value],
        )?;
    }
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync job handlers (one per resource type)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpscout::{
        HsCustomerAddress, HsCustomerEmail, HsCustomerPhone, HsCustomerPropertyValue,
        HsCustomerSocialProfile, HsCustomerWebsite,
    };
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        // The canonical boot chain — conversations.state/type/source_* land
        // with the later batches, so the upserts need the full chain.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    #[test]
    fn upsert_mailbox_round_trips() {
        let conn = fresh_db();
        let m = HsMailbox {
            remote_id: 101,
            name: "General Support".into(),
            slug: Some("general".into()),
            email: Some("support@example.com".into()),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
        };
        upsert_mailbox(&conn, &m).unwrap();

        let (name, email): (String, Option<String>) = conn
            .query_row(
                "SELECT name, email FROM mailboxes WHERE remote_id = 101",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "General Support");
        assert_eq!(email, Some("support@example.com".into()));
    }

    #[test]
    fn upsert_conversation_round_trips() {
        let conn = fresh_db();
        let c = HsConversation {
            remote_id: 1001,
            number: 1001,
            kind: Some("email".into()),
            source_type: None,
            source_via: None,
            subject: Some("Test".into()),
            preview: Some("Preview".into()),
            status: "active".into(),
            state: Some("published".into()),
            mailbox_id: 101,
            assignee_id: Some(1),
            assignee_type: Some("user".into()),
            assigned_team_id: None,
            customer_id: 2001,
            priority: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T12:00:00Z".into()),
            closed_at: None,
            snoozed_until: None,
            thread_count: 2,
            merged_into: None,
            tags: vec!["timezone".into(), "vip".into()],
        };
        upsert_conversation(&conn, &c).unwrap();

        let (subject, status): (Option<String>, String) = conn
            .query_row(
                "SELECT subject, status FROM conversations WHERE remote_id = 1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(subject, Some("Test".into()));
        assert_eq!(status, "active");
        // The v1.3.0 channel + state/thread-count columns persist.
        let (kind, state, thread_count): (Option<String>, Option<String>, i64) = conn
            .query_row(
                "SELECT type, state, thread_count FROM conversations WHERE remote_id = 1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, Some("email".into()));
        assert_eq!(state, Some("published".into()));
        assert_eq!(thread_count, 2);
    }

    #[test]
    fn upsert_conversation_maps_remote_ids_to_local_mirrors() {
        let conn = fresh_db();
        // Land the reference data first (reference sync order: users,
        // mailboxes, customers before conversations).
        upsert_user(
            &conn,
            &HsUser {
                remote_id: 1,
                first_name: Some("Demo".into()),
                last_name: Some("Agent".into()),
                email: Some("demo@support.test".into()),
                role: Some("owner".into()),
                user_type: "user".into(),
                timezone: None,
                photo_url: None,
                initials: None,
                mention: None,
                job_title: None,
                phone: None,
                alternate_emails: vec![],
                created_at: None,
                updated_at: None,
            },
        )
        .unwrap();
        upsert_mailbox(
            &conn,
            &HsMailbox {
                remote_id: 101,
                name: "Support".into(),
                slug: Some("support".into()),
                email: None,
                created_at: None,
                updated_at: None,
            },
        )
        .unwrap();
        upsert_customer(
            &conn,
            &HsCustomer {
                remote_id: 2001,
                first_name: Some("Alice".into()),
                last_name: Some("Wonderland".into()),
                email: Some("alice@example.com".into()),
                organization: None,
                job_title: None,
                phone: None,
                created_at: None,
                updated_at: None,
                photo_url: None,
                organization_id: None,
                background: None,
                age: None,
                gender: None,
                location: None,
                emails: vec![],
                phones: vec![],
                websites: vec![],
                social_profiles: vec![],
                address: None,
                properties: vec![],
            },
        )
        .unwrap();
        upsert_conversation(
            &conn,
            &HsConversation {
                remote_id: 1001,
                number: 1001,
                kind: None,
                source_type: None,
                source_via: None,
                subject: Some("Local ids".into()),
                preview: None,
                status: "active".into(),
                state: None,
                mailbox_id: 101,
                assignee_id: Some(1),
                assignee_type: None,
                assigned_team_id: None,
                customer_id: 2001,
                priority: None,
                created_at: None,
                updated_at: None,
                closed_at: None,
                snoozed_until: None,
                thread_count: 0,
                merged_into: None,
                tags: vec![],
            },
        )
        .unwrap();

        // The stored ids are the LOCAL mirror ids, so the join the
        // customer/search/timeline queries rely on resolves.
        let (mailbox, assignee, customer): (i64, Option<i64>, i64) = conn
            .query_row(
                "SELECT mailbox_id, assignee_id, customer_id
                 FROM conversations WHERE remote_id = 1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let mailbox_local: i64 = conn
            .query_row("SELECT id FROM mailboxes WHERE remote_id = 101", [], |r| {
                r.get(0)
            })
            .unwrap();
        let user_local: i64 = conn
            .query_row("SELECT id FROM users WHERE remote_id = 1", [], |r| r.get(0))
            .unwrap();
        let customer_local: i64 = conn
            .query_row("SELECT id FROM customers WHERE remote_id = 2001", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(mailbox, mailbox_local);
        assert_eq!(assignee, Some(user_local));
        assert_eq!(customer, customer_local);
        // And the customer join actually finds the conversation.
        let joined: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversations c
                 JOIN customers cu ON cu.id = c.customer_id
                 WHERE cu.remote_id = 2001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(joined, 1);
    }

    #[test]
    fn upsert_conversation_keeps_remote_ids_when_mirror_missing() {
        let conn = fresh_db();
        // No customer/mailbox/user rows: the landing must not drop the
        // conversation (NOT NULL columns keep the remote id).
        upsert_conversation(
            &conn,
            &HsConversation {
                remote_id: 1002,
                number: 1002,
                kind: None,
                source_type: None,
                source_via: None,
                subject: None,
                preview: None,
                status: "active".into(),
                state: None,
                mailbox_id: 101,
                assignee_id: None,
                assignee_type: None,
                assigned_team_id: None,
                customer_id: 2002,
                priority: None,
                created_at: None,
                updated_at: None,
                closed_at: None,
                snoozed_until: None,
                thread_count: 0,
                merged_into: None,
                tags: vec![],
            },
        )
        .unwrap();
        let (mailbox, customer): (i64, i64) = conn
            .query_row(
                "SELECT mailbox_id, customer_id FROM conversations WHERE remote_id = 1002",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mailbox, 101);
        assert_eq!(customer, 2002);
    }

    #[test]
    fn upsert_customer_round_trips() {
        let conn = fresh_db();
        let c = HsCustomer {
            remote_id: 2001,
            first_name: Some("Alice".into()),
            last_name: Some("Wonderland".into()),
            email: Some("alice@example.com".into()),
            organization: Some("Acme".into()),
            job_title: Some("Engineer".into()),
            phone: Some("+1-555-0001".into()),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            photo_url: None,
            organization_id: None,
            background: None,
            age: None,
            gender: None,
            location: None,
            emails: vec![],
            phones: vec![],
            websites: vec![],
            social_profiles: vec![],
            address: None,
            properties: vec![],
        };
        upsert_customer(&conn, &c).unwrap();

        let (first, email): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT first_name, email FROM customers WHERE remote_id = 2001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(first, Some("Alice".into()));
        assert_eq!(email, Some("alice@example.com".into()));
    }

    /// SY-05 (C8): the contact side tables + property values are part of the
    /// provider's customer object and must land in the mirror.
    #[test]
    fn upsert_customer_persists_side_tables_and_properties() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO customer_property_definitions (remote_id, name, slug, type, sort_order)
             VALUES (4101, 'Plan', 'plan', 'dropdown', 1)",
            [],
        )
        .unwrap();
        let c = HsCustomer {
            remote_id: 2002,
            first_name: Some("Ada".into()),
            last_name: Some("Lovelace".into()),
            email: Some("ada@work.example".into()),
            organization: None,
            job_title: None,
            phone: Some("+1-555-0002".into()),
            created_at: None,
            updated_at: None,
            photo_url: None,
            organization_id: None,
            background: None,
            age: None,
            gender: None,
            location: None,
            emails: vec![
                HsCustomerEmail {
                    value: Some("ada@work.example".into()),
                    kind: Some("work".into()),
                },
                HsCustomerEmail {
                    value: Some("ada@home.example".into()),
                    kind: Some("home".into()),
                },
            ],
            phones: vec![HsCustomerPhone {
                value: Some("+1-555-0002".into()),
                kind: Some("work".into()),
            }],
            websites: vec![HsCustomerWebsite {
                value: Some("https://ada.example".into()),
            }],
            social_profiles: vec![HsCustomerSocialProfile {
                value: Some("https://linkedin.com/in/ada".into()),
                kind: Some("linkedin".into()),
            }],
            address: Some(HsCustomerAddress {
                line1: Some("1 Analytical Way".into()),
                line2: None,
                city: Some("London".into()),
                state: None,
                postal_code: Some("SW1".into()),
                country: Some("UK".into()),
            }),
            properties: vec![
                HsCustomerPropertyValue {
                    definition_remote_id: Some(4101),
                    key: None,
                    name: None,
                    value: Some("Pro".into()),
                },
                // Unknown definition -> stub definition from the value's name.
                HsCustomerPropertyValue {
                    definition_remote_id: Some(4105),
                    key: Some("region".into()),
                    name: Some("Region".into()),
                    value: Some("EMEA".into()),
                },
            ],
        };
        upsert_customer(&conn, &c).unwrap();

        let local: i64 = conn
            .query_row("SELECT id FROM customers WHERE remote_id = 2002", [], |r| {
                r.get(0)
            })
            .unwrap();
        // Emails with types.
        let emails: Vec<(String, Option<String>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT value, type FROM customer_emails WHERE customer_id = ?1 ORDER BY value",
                )
                .unwrap();
            stmt.query_map(params![local], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert_eq!(
            emails,
            vec![
                ("ada@home.example".to_string(), Some("home".to_string())),
                ("ada@work.example".to_string(), Some("work".to_string())),
            ],
            "both emails persisted with their types"
        );
        // Phone / website / social.
        let (phone, ptype): (String, Option<String>) = conn
            .query_row(
                "SELECT value, type FROM customer_phones WHERE customer_id = ?1",
                params![local],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(phone, "+1-555-0002");
        assert_eq!(ptype.as_deref(), Some("work"));
        let site: String = conn
            .query_row(
                "SELECT value FROM customer_websites WHERE customer_id = ?1",
                params![local],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(site, "https://ada.example");
        let (social, stype): (String, Option<String>) = conn
            .query_row(
                "SELECT value, type FROM customer_social_profiles WHERE customer_id = ?1",
                params![local],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(social, "https://linkedin.com/in/ada");
        assert_eq!(stype.as_deref(), Some("linkedin"));
        // Address.
        let (lines, city, postal, country): (Option<String>, Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT lines, city, postal_code, country FROM customer_addresses WHERE customer_id = ?1",
                params![local],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(lines.as_deref(), Some("1 Analytical Way"));
        assert_eq!(city.as_deref(), Some("London"));
        assert_eq!(postal.as_deref(), Some("SW1"));
        assert_eq!(country.as_deref(), Some("UK"));
        // Properties: the known definition resolves; the unknown one creates
        // a stub definition from the value's own name.
        let props: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT d.name, p.value FROM customer_properties p
                       JOIN customer_property_definitions d ON d.id = p.definition_id
                      WHERE p.customer_id = ?1 ORDER BY d.name",
                )
                .unwrap();
            stmt.query_map(params![local], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert_eq!(
            props,
            vec![
                ("Plan".to_string(), "Pro".to_string()),
                ("Region".to_string(), "EMEA".to_string()),
            ],
            "property values land with their definitions (stub when unknown)"
        );

        // Replace-set semantics: a re-upsert with a single email replaces the
        // set (no duplicates, removals propagate), and a changed property
        // value updates in place.
        let mut c2 = c.clone();
        c2.emails.truncate(1);
        c2.properties[0].value = Some("Enterprise".into());
        upsert_customer(&conn, &c2).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM customer_emails WHERE customer_id = ?1",
                params![local],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "the removed email is gone, no duplicates");
        let plan_value: String = conn
            .query_row(
                "SELECT p.value FROM customer_properties p
                   JOIN customer_property_definitions d ON d.id = p.definition_id
                  WHERE p.customer_id = ?1 AND d.remote_id = 4101",
                params![local],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(plan_value, "Enterprise", "property value updated in place");
    }

    #[test]
    fn upsert_user_round_trips() {
        let conn = fresh_db();
        let u = HsUser {
            remote_id: 1,
            first_name: Some("Demo".into()),
            last_name: Some("Agent".into()),
            email: Some("demo@test.com".into()),
            role: Some("owner".into()),
            user_type: "user".into(),
            timezone: Some("UTC".into()),
            photo_url: None,
            initials: Some("DA".into()),
            mention: Some("@demo".into()),
            job_title: Some("Lead".into()),
            phone: None,
            alternate_emails: vec![],
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
        };
        upsert_user(&conn, &u).unwrap();

        let email: Option<String> = conn
            .query_row("SELECT email FROM users WHERE remote_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(email, Some("demo@test.com".into()));
    }

    #[test]
    fn upsert_tag_round_trips() {
        let conn = fresh_db();
        let t = HsTag {
            remote_id: 301,
            name: "bug".into(),
            slug: Some("bug".into()),
            color: Some("#f85149".into()),
            ticket_count: Some(3),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
        };
        upsert_tag(&conn, &t).unwrap();

        let name: String = conn
            .query_row("SELECT name FROM tags WHERE remote_id = 301", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "bug");
    }

    #[test]
    fn upsert_team_round_trips() {
        let conn = fresh_db();
        let t = HsTeam {
            remote_id: 201,
            name: "Support Team".into(),
            member_user_ids: vec![1, 2, 3],
        };
        upsert_team(&conn, &t).unwrap();

        let name: String = conn
            .query_row("SELECT name FROM teams WHERE remote_id = 201", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "Support Team");
    }

    #[test]
    fn upsert_mailbox_upserts_on_second_call() {
        let conn = fresh_db();
        let m1 = HsMailbox {
            remote_id: 101,
            name: "General".into(),
            slug: None,
            email: Some("old@example.com".into()),
            created_at: None,
            updated_at: None,
        };
        upsert_mailbox(&conn, &m1).unwrap();

        let m2 = HsMailbox {
            remote_id: 101,
            name: "General Support".into(),
            slug: Some("general".into()),
            email: Some("new@example.com".into()),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
        };
        upsert_mailbox(&conn, &m2).unwrap();

        let (name, email): (String, Option<String>) = conn
            .query_row(
                "SELECT name, email FROM mailboxes WHERE remote_id = 101",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "General Support");
        assert_eq!(email, Some("new@example.com".into()));
    }
}
