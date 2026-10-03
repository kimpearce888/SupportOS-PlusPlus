//! Sync mirror write-layer — cursor/checkpoint helpers + the Help Scout
//! upsert helpers the SyncEngine (sync_engine.rs) and the worker job
//! executor share. The coordinator itself (initial/incremental/reconcile)
//! lives in `sync_engine.rs`; this module owns the SQL that lands rows.

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::helpscout::{HsConversation, HsCustomer, HsMailbox, HsTag, HsTeam, HsUser};

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

/// Upsert a team into the `teams` table.
pub fn upsert_team(conn: &Connection, t: &HsTeam) -> Result<()> {
    conn.execute(
        "INSERT INTO teams (remote_id, name)
         VALUES (?1, ?2)
         ON CONFLICT(remote_id) DO UPDATE SET name = excluded.name",
        params![t.remote_id, t.name],
    )?;
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
    conn.execute(
        "INSERT INTO conversations (remote_id, number, subject, preview, status, mailbox_id,
            assignee_id, customer_id, priority, created_at, updated_at, closed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(remote_id) DO UPDATE SET
            number = excluded.number,
            subject = excluded.subject,
            preview = excluded.preview,
            status = excluded.status,
            mailbox_id = excluded.mailbox_id,
            assignee_id = excluded.assignee_id,
            customer_id = excluded.customer_id,
            priority = excluded.priority,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            closed_at = excluded.closed_at",
        params![
            c.remote_id,
            c.number,
            c.subject,
            c.preview,
            c.status,
            mailbox_local.unwrap_or(c.mailbox_id),
            assignee_local,
            customer_local.unwrap_or(c.customer_id),
            c.priority,
            c.created_at,
            c.updated_at,
            c.closed_at,
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
    Ok(())
}

/// Upsert a customer into the `customers` table.
pub fn upsert_customer(conn: &Connection, c: &HsCustomer) -> Result<()> {
    conn.execute(
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
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync job handlers (one per resource type)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
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
            subject: Some("Test".into()),
            preview: Some("Preview".into()),
            status: "active".into(),
            mailbox_id: 101,
            assignee_id: Some(1),
            customer_id: 2001,
            priority: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T12:00:00Z".into()),
            closed_at: None,
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
            },
        )
        .unwrap();
        upsert_conversation(
            &conn,
            &HsConversation {
                remote_id: 1001,
                number: 1001,
                subject: Some("Local ids".into()),
                preview: None,
                status: "active".into(),
                mailbox_id: 101,
                assignee_id: Some(1),
                customer_id: 2001,
                priority: None,
                created_at: None,
                updated_at: None,
                closed_at: None,
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
                subject: None,
                preview: None,
                status: "active".into(),
                mailbox_id: 101,
                assignee_id: None,
                customer_id: 2002,
                priority: None,
                created_at: None,
                updated_at: None,
                closed_at: None,
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
