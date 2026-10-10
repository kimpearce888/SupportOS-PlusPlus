//! Mirror-schema parity migrations (DB-02 / DB-03 / DB-04 / DB-06).
//!
//! The port's mirror schema diverged from the reference
//! (`src/server/database/migrations/001..016` @ c346fb51) in four audit
//! findings, fixed here as forward-only versioned boot steps:
//!
//! * **M044 (DB-02)** — the 123-table mirror schema: `docs` gains the
//!   reference name `docs_articles`, the undocumented
//!   `issue_cluster_members` shape becomes the reference
//!   `issue_cluster_conversations`, and the `encrypted_sync_log` ledger
//!   joins the boot chain instead of appearing lazily on first export.
//! * **M045 (DB-04)** — `conversation_threads` (the documented rename of
//!   the reference `threads`) gets the reference actor model:
//!   `from_type` + `created_by_user_id` / `created_by_customer_id` /
//!   `created_by_system_user_id` instead of the collapsed
//!   `actor_id` + `actor_type`, plus the `type` / `body_text` column names.
//! * **M047 (DB-03)** — `conversations` restores the reference column
//!   names (`mailbox_local_id` / `assignee_local_id` / `customer_local_id`),
//!   `UNIQUE(number)` and the reference FK set.
//!
//! ## The rebuild machinery
//!
//! SQLite cannot `ALTER TABLE ... ADD CONSTRAINT`, so declaring FKs or
//! renaming columns with constraints means rebuilding the table. The
//! reference-safe dance (SQLite's documented 12-step procedure, adapted
//! to the port's DB-01 "one transaction per boot step" rule, which forbids
//! `PRAGMA foreign_keys=OFF` — a no-op inside a transaction):
//!
//! 1. `PRAGMA defer_foreign_keys=ON` — legal inside a transaction; FK
//!    checks move to commit.
//! 2. Capture the child rows — the rows of tables holding an FK into
//!    `{t}`. (SQLite >= 3.25 *always* rewrites the FK clauses of other
//!    tables on `RENAME` — `PRAGMA legacy_alter_table` no longer
//!    suppresses that — so the old `__old`/swap dance would leave the
//!    children pointing at a dropped transient name. Instead: DROP the
//!    original outright and RENAME the rebuild in. `DROP TABLE` performs
//!    an implicit `DELETE FROM`, which fires the children's cascades —
//!    hence the capture; the children's FK clauses name the FINAL table,
//!    which exists again after the rename, so they need no repair.)
//! 3. `CREATE TABLE "{t}__rebuild" (...reference shape...)` — the FK
//!    clauses name the final table, so they resolve to the old table
//!    during the copy (same ids) and to the new one after the swap.
//! 4. `INSERT INTO "{t}__rebuild" SELECT ...` — the copy *converges* rows
//!    to the FK invariant the reference always enforced: invalid
//!    `SET NULL`-style references become NULL, orphaned `CASCADE` rows
//!    (children of deleted parents) are dropped — what the cascade would
//!    have done all along. Nothing silently changes shape.
//! 5. `DROP TABLE "{t}"`, then
//!    `ALTER TABLE "{t}__rebuild" RENAME TO "{t}"`.
//! 6. Recreate the captured indexes (the old table's died with it), with
//!    expressions updated where the rebuild renamed columns.
//! 7. Restore the captured child rows — keeping only rows whose parent
//!    row survived the converging copy (the rest are what the cascade
//!    would have removed).

use rusqlite::Connection;

use crate::error::Result;

/// One table rebuild: the new shape + the copy that converges old rows.
struct Rebuild<'a> {
    /// The final table name. Same as the old name for an in-place shape
    /// rebuild; different for a rename-rebuild (DB-02's
    /// `issue_cluster_members` -> `issue_cluster_conversations`).
    new_name: &'a str,
    /// Body of the new `CREATE TABLE` (column list + constraints).
    new_ddl: &'a str,
    /// Full `INSERT INTO ... SELECT ...` from the OLD table into the new
    /// one (the converging copy).
    copy_sql: &'a str,
    /// Indexes whose expressions reference renamed columns get explicit
    /// replacement DDL here (name-colliding captured indexes are skipped).
    replacement_indexes: &'a [&'a str],
}

/// Rename a column-preserving index definition to `IF NOT EXISTS` form.
fn with_if_not_exists(sql: &str) -> String {
    let s = sql.trim().trim_end_matches(';');
    if let Some(rest) = s.strip_prefix("CREATE UNIQUE INDEX ") {
        format!("CREATE UNIQUE INDEX IF NOT EXISTS {rest}")
    } else if let Some(rest) = s.strip_prefix("CREATE INDEX ") {
        format!("CREATE INDEX IF NOT EXISTS {rest}")
    } else {
        s.to_string()
    }
}

/// Does the table exist?
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// The captured (named, non-internal) index DDL of a table.
fn captured_indexes(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name, sql FROM sqlite_master
          WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([table], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows.into_iter().map(|(_, sql)| sql).collect())
}

/// The captured trigger DDL of a table (production keeps none today; the
/// capture keeps the machinery honest if that changes).
fn captured_triggers(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1 AND sql IS NOT NULL",
    )?;
    let rows: Vec<String> = stmt
        .query_map([table], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// One captured child table: the rows (and the FK map needed to filter
/// them) of a table holding a foreign key into the rebuilt table.
struct ChildRows {
    name: String,
    /// `(from_column, parent_table, to_column)` per FK clause that targets
    /// the rebuilt table (positional row values pair up with the table's
    /// column list from `PRAGMA table_info`).
    fks: Vec<(String, String, String)>,
    /// Column names of the child, in row order.
    columns: Vec<String>,
    rows: Vec<Vec<rusqlite::types::Value>>,
}

/// The tables holding a foreign key into `table` (its FK children).
fn fk_children(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master
          WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    let mut children = Vec::new();
    for name in names {
        // The FK clauses naming `table` (foreign_key_list's "table" column).
        let mut fk = conn.prepare(&format!("PRAGMA foreign_key_list({name})"))?;
        let hits: i64 = fk
            .query_map([], |r| r.get::<_, String>(2))?
            .filter_map(|r| r.ok())
            .filter(|t| t == table)
            .count() as i64;
        if hits > 0 {
            children.push(name);
        }
    }
    Ok(children)
}

/// Capture one child table's rows + the FK map into the rebuilt table.
fn capture_child(conn: &Connection, name: &str, parent: &str) -> Result<ChildRows> {
    let columns: Vec<String> = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({name})"))?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get(1))?
            .filter_map(|c| c.ok())
            .collect();
        cols
    };
    let fks: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(&format!("PRAGMA foreign_key_list({name})"))?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .filter(|(from, to, _)| to == parent && from != to)
            .collect();
        rows
    };
    let rows: Vec<Vec<rusqlite::types::Value>> = {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {name}"))?;
        let n = columns.len();
        let rows = stmt
            .query_map([], |r| {
                let mut row: Vec<rusqlite::types::Value> = Vec::with_capacity(n);
                for i in 0..n {
                    row.push(r.get(i)?);
                }
                Ok(row)
            })?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    Ok(ChildRows {
        name: name.to_string(),
        fks,
        columns,
        rows,
    })
}

/// Restore the captured child rows after the swap — keeping only rows
/// whose parent row survived the converging copy (what the cascade would
/// have removed stays removed).
fn restore_child(conn: &Connection, cap: &ChildRows) -> Result<()> {
    let placeholders = (1..=cap.columns.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        cap.name,
        cap.columns.join(", "),
        placeholders
    );
    'rows: for row in &cap.rows {
        for (from, parent, to) in &cap.fks {
            let idx = cap
                .columns
                .iter()
                .position(|c| c == from)
                .expect("foreign_key_list column exists in table_info");
            if let rusqlite::types::Value::Integer(id) = &row[idx] {
                let n: i64 = conn.query_row(
                    &format!("SELECT COUNT(*) FROM {parent} WHERE {to} = ?1"),
                    [id],
                    |r| r.get(0),
                )?;
                if n == 0 {
                    // The parent row converged away — the child row is what
                    // the cascade would have deleted.
                    continue 'rows;
                }
            }
        }
        conn.execute(&sql, rusqlite::params_from_iter(row.iter().cloned()))?;
    }
    Ok(())
}

/// Run the reference-shape rebuild for one table.
///
/// **In-place rebuild** (`rb.new_name == table`, the FK/renamed-column
/// cases): capture the FK children's rows, create the `__rebuild` table,
/// run the converging copy, DROP the original (its implicit DELETE fires
/// the children's cascades — the captured rows come back after the swap),
/// RENAME the rebuild in, replay the captured indexes minus the replaced
/// ones, then restore the child rows that still have a parent. The
/// children's FK clauses name the FINAL table (a `DROP TABLE` rewrites
/// nothing), so they need no repair.
///
/// **Rename-rebuild** (`rb.new_name != table`): create the final name,
/// copy, drop the old — nothing references the old name (its own outgoing
/// FKs are irrelevant to a drop).
///
/// Idempotent by caller contract: only invoke it when the OLD shape is
/// detected (each step's apply function checks first).
fn rebuild_table(conn: &Connection, table: &str, rb: &Rebuild<'_>) -> Result<()> {
    // FK enforcement is deferred to commit; the converging copy keeps the
    // committed state valid either way (DB-01 runs each step in one
    // transaction, and `PRAGMA foreign_keys=OFF` is a no-op inside one).
    conn.execute_batch("PRAGMA defer_foreign_keys = ON;")?;

    let indexes = captured_indexes(conn, table)?;
    let triggers = captured_triggers(conn, table)?;
    let replaced: Vec<String> = rb
        .replacement_indexes
        .iter()
        .map(|s| s.to_string())
        .collect();
    // Index names that the replacements own — captured DDL for those names
    // is dropped in favor of the (possibly renamed-column) replacement.
    let replaced_names: Vec<String> = rb
        .replacement_indexes
        .iter()
        .filter_map(|ddl| {
            ddl.split_whitespace()
                .position(|w| w.eq_ignore_ascii_case("index"))
                .and_then(|p| ddl.split_whitespace().nth(p + 1))
                .map(|n| n.trim_matches('"').to_string())
        })
        .collect();

    if rb.new_name == table {
        // In-place: DROP + RENAME-in (see the module doc — the `__old`
        // swap dance dies on SQLite >= 3.25, which always rewrites the
        // children's FK clauses on RENAME). The children's rows are
        // captured around the DROP's cascades.
        let children: Vec<ChildRows> = fk_children(conn, table)?
            .iter()
            .map(|name| capture_child(conn, name, table))
            .collect::<Result<Vec<_>>>()?;
        conn.execute_batch(&format!(
            "CREATE TABLE \"{}__rebuild\" {};",
            table, rb.new_ddl
        ))?;
        conn.execute_batch(rb.copy_sql)?;
        conn.execute_batch(&format!("DROP TABLE \"{}\";", table))?;
        conn.execute_batch(&format!(
            "ALTER TABLE \"{}__rebuild\" RENAME TO \"{table}\";",
            table
        ))?;
        // Recreate the captured indexes (the old table's died with it),
        // skipping the names the replacements own, then the replacements.
        for ddl in &indexes {
            let name = ddl
                .split_whitespace()
                .position(|w| w.eq_ignore_ascii_case("index"))
                .and_then(|p| ddl.split_whitespace().nth(p + 1))
                .map(|n| n.trim_matches('"').to_string())
                .unwrap_or_default();
            if !replaced_names.iter().any(|n| n == &name) {
                conn.execute_batch(&with_if_not_exists(ddl))?;
            }
        }
        for ddl in &replaced {
            conn.execute_batch(&with_if_not_exists(ddl))?;
        }
        for ddl in &triggers {
            // Triggers were captured while attached to the old table; their
            // bodies name the table, whose name is restored after the swap.
            conn.execute_batch(ddl)?;
        }
        for cap in &children {
            restore_child(conn, cap)?;
        }
    } else {
        // Rename-rebuild: the final name is free, the old name has no
        // referrers. The old table's captured indexes die with it — the
        // caller's `replacement_indexes` carries the index set the new
        // table needs (reference names on the reference table).
        conn.execute_batch(&format!("CREATE TABLE \"{}\" {};", rb.new_name, rb.new_ddl))?;
        conn.execute_batch(rb.copy_sql)?;
        conn.execute_batch(&format!("DROP TABLE \"{}\";", table))?;
        for ddl in &replaced {
            conn.execute_batch(&with_if_not_exists(ddl))?;
        }
    }

    Ok(())
}

/// M044 — DB-02: complete the 123-table mirror schema.
///
/// * `docs` → `docs_articles` (the reference name; data-preserving rename,
///   the port's reference-shaped columns stay).
/// * `issue_cluster_members` → `issue_cluster_conversations` (the
///   reference shape, with the `conversation_id` FK the undocumented port
///   table never declared; orphaned links converge away — the CASCADE the
///   reference always enforced).
/// * `encrypted_sync_log` ensured in the boot chain (reference migration
///   009; the port created it lazily on first export).
pub fn apply_m044(conn: &Connection) -> Result<()> {
    // ---- docs -> docs_articles -------------------------------------------
    // Plain rename: no FKs point at `docs`, `docs_fts` is standalone and
    // maintained by id, and the table's indexes follow the rename.
    if table_exists(conn, "docs")? {
        if table_exists(conn, "docs_articles")? {
            // A previous pass already converted (the DB-01 adopt-the-
            // versions re-run): this `docs` is an empty stray recreated by
            // the idempotent M029 batch earlier in the same boot. Nothing
            // writes to `docs` anymore — drop the stray.
            conn.execute_batch("DROP TABLE docs;")?;
        } else {
            conn.execute_batch(
                "ALTER TABLE docs RENAME TO docs_articles;
                 CREATE INDEX IF NOT EXISTS idx_docs_articles_collection
                     ON docs_articles (collection_local_id);
                 CREATE INDEX IF NOT EXISTS idx_docs_articles_status
                     ON docs_articles (status);",
            )?;
        }
    }

    // ---- issue_cluster_members -> issue_cluster_conversations -----------
    let members_exists = table_exists(conn, "issue_cluster_members")?;
    if members_exists {
        if table_exists(conn, "issue_cluster_conversations")? {
            // Already converted (the DB-01 adopt-the-versions re-run): the
            // idempotent M016 batch recreated an empty stray — drop it.
            conn.execute_batch("DROP TABLE issue_cluster_members;")?;
        } else {
            rebuild_table(
                conn,
                "issue_cluster_members",
                &Rebuild {
                    new_name: "issue_cluster_conversations",
                    new_ddl: "(
                        cluster_id      INTEGER NOT NULL
                            REFERENCES issue_clusters (id) ON DELETE CASCADE,
                        conversation_id INTEGER NOT NULL
                            REFERENCES conversations (id) ON DELETE CASCADE,
                        assigned_at     TEXT NOT NULL DEFAULT (datetime('now')),
                        PRIMARY KEY (cluster_id, conversation_id)
                    )",
                    copy_sql: "INSERT INTO issue_cluster_conversations
                                    (cluster_id, conversation_id, assigned_at)
                                SELECT m.cluster_id, m.conversation_id, m.assigned_at
                                  FROM issue_cluster_members m
                                 WHERE m.cluster_id IN (SELECT id FROM issue_clusters)
                                   AND m.conversation_id IN (SELECT id FROM conversations)",
                    replacement_indexes: &[
                        "CREATE INDEX idx_issue_cluster_conversations_conversation
                             ON issue_cluster_conversations (conversation_id)",
                    ],
                },
            )?;
        }
    }

    // ---- encrypted_sync_log in the boot chain ---------------------------
    // Reference migration 009 creates it at boot; the port created it
    // lazily on first export (BK-04's bundle guard compensates, but the
    // table belongs to the schema, not the export path).
    crate::encrypted_sync::ensure_log_table(conn)?;

    let _ = conn.execute("UPDATE app_state SET schema_version = 44 WHERE id = 1", []);
    Ok(())
}

/// M045 — DB-04: the reference `threads` actor model on `conversation_threads`
/// (the documented rename of `threads`).
///
/// The port collapsed the reference's actor model — `from_type` plus the
/// three-way `created_by_user_id` / `created_by_customer_id` /
/// `created_by_system_user_id` split — into a single `actor_id` +
/// `actor_type` pair, and renamed `type` to `thread_type` and `body_text` to
/// `body`. M045 restores the reference columns and their FK set:
///
/// * `thread_type` -> `type`, `body` -> `body_text`, `actor_type` ->
///   `from_type` (the port's `'system'` value converges to the reference's
///   `'system_user'`).
/// * `actor_id` splits three ways by actor kind; ids that do not resolve
///   to a live `users` / `customers` / `system_users` row become NULL
///   (the SET-NULL-able semantics the reference FKs always enforced).
/// * `conversation_id` gains the reference `ON DELETE CASCADE` FK —
///   threads of deleted conversations converge away (what the cascade
///   would have removed all along).
/// * `scheduled_for` joins the shape (reference 001; SY-10's scheduler
///   already carries the value on the wire) — NULL for existing rows.
///
/// The port's additive extras (`created_at`, the named
/// `idx_conv_threads_remote` UNIQUE index instead of the inline column
/// constraint) stay.
pub fn apply_m045(conn: &Connection) -> Result<()> {
    if crate::sync_schema::column_exists(conn, "conversation_threads", "actor_id")? {
        rebuild_table(
            conn,
            "conversation_threads",
            &Rebuild {
                new_name: "conversation_threads",
                new_ddl: "(
                    id                        INTEGER PRIMARY KEY AUTOINCREMENT,
                    remote_id                 INTEGER,
                    conversation_id           INTEGER NOT NULL
                                                  REFERENCES conversations (id) ON DELETE CASCADE,
                    type                      TEXT,
                    state                     TEXT DEFAULT 'published',
                    body_text                 TEXT,
                    body_html                 TEXT,
                    from_name                 TEXT,
                    from_email                TEXT,
                    from_type                 TEXT,
                    created_by_user_id        INTEGER REFERENCES users (id),
                    created_by_customer_id    INTEGER REFERENCES customers (id),
                    created_by_system_user_id INTEGER REFERENCES system_users (id),
                    assigned_to_type         TEXT,
                    assigned_to_id            INTEGER,
                    saved_reply_local_id      INTEGER REFERENCES saved_replies (id),
                    action_type               TEXT,
                    action_text               TEXT,
                    to_list                   TEXT,
                    cc_list                   TEXT,
                    bcc_list                  TEXT,
                    scheduled_for             TEXT,
                    remote_created_at         TEXT,
                    remote_updated_at         TEXT,
                    local_created_at           TEXT NOT NULL DEFAULT (datetime('now')),
                    local_updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
                    last_synced_at            TEXT,
                    raw_json                  TEXT,
                    raw_json_hash             TEXT,
                    deleted_at                TEXT,
                    fts_indexed               INTEGER DEFAULT 0,
                    embedding_state           TEXT DEFAULT 'not_indexed',
                    created_at                TEXT NOT NULL
                                                  DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                )",
                copy_sql: "INSERT INTO \"conversation_threads__rebuild\"
                               (id, remote_id, conversation_id, type, state,
                                body_text, body_html, from_name, from_email, from_type,
                                created_by_user_id, created_by_customer_id,
                                created_by_system_user_id,
                                assigned_to_type, assigned_to_id, saved_reply_local_id,
                                action_type, action_text, to_list, cc_list, bcc_list,
                                scheduled_for, remote_created_at, remote_updated_at,
                                local_created_at, local_updated_at, last_synced_at,
                                raw_json, raw_json_hash, deleted_at, fts_indexed,
                                embedding_state, created_at)
                           SELECT t.id, t.remote_id, t.conversation_id,
                                  t.thread_type, t.state, t.body, t.body_html,
                                  t.from_name, t.from_email,
                                  CASE WHEN t.actor_type = 'system'
                                       THEN 'system_user' ELSE t.actor_type END,
                                  CASE WHEN t.actor_type = 'user'
                                            AND t.actor_id IN (SELECT id FROM users)
                                       THEN t.actor_id END,
                                  CASE WHEN t.actor_type = 'customer'
                                            AND t.actor_id IN (SELECT id FROM customers)
                                       THEN t.actor_id END,
                                  CASE WHEN t.actor_type IN ('system', 'system_user')
                                            AND t.actor_id IN (SELECT id FROM system_users)
                                       THEN t.actor_id END,
                                  t.assigned_to_type, t.assigned_to_id,
                                  CASE WHEN t.saved_reply_local_id
                                             IN (SELECT id FROM saved_replies)
                                       THEN t.saved_reply_local_id END,
                                  t.action_type, t.action_text,
                                  t.to_list, t.cc_list, t.bcc_list,
                                  t.scheduled_for,
                                  t.remote_created_at, t.remote_updated_at,
                                  t.local_created_at, t.local_updated_at,
                                  t.last_synced_at, t.raw_json, t.raw_json_hash,
                                  t.deleted_at, t.fts_indexed, t.embedding_state,
                                  t.created_at
                             FROM conversation_threads t
                            WHERE t.conversation_id IN (SELECT id FROM conversations)",
                replacement_indexes: &[
                    // The captured expression names the renamed column
                    // (written without IF NOT EXISTS — the replay adds it).
                    "CREATE INDEX idx_conv_threads_type
                         ON conversation_threads (type)",
                ],
            },
        )?;
    }
    let _ = conn.execute("UPDATE app_state SET schema_version = 45 WHERE id = 1", []);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn booted() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    /// The 123-table contract: every reference migration table exists in
    /// the port under its documented rename (threads->conversation_threads,
    /// segments->saved_segments, support_graph_edges->graph_edges,
    /// customer_memories->customer_memory,
    /// conversation_events->activity_events,
    /// knowledge_candidates->knowledge_gap_candidates), plus the runtime
    /// `golden_test_set` the reference's aiRepo ensures.
    #[test]
    fn m044_completes_the_reference_table_set() {
        let conn = booted();
        // The reference's 123 migration tables (001..016), port names.
        let reference = [
            "accounts",
            "ai_attributes",
            "ai_drafts",
            "ai_extracted_facts",
            "ai_feedback",
            "ai_runs",
            "ai_sources",
            "ai_verifications",
            "application_errors",
            "application_settings",
            "attachments",
            "audit_log",
            "automation_rules",
            "automation_runs",
            "client_behavior_baselines",
            "client_behavior_observations",
            "client_communication_preferences",
            "client_current_signals",
            "client_human_overrides",
            "client_support_outcomes",
            "coaching_reviews",
            "connector_rows",
            "connectors",
            "conversation_chunks",
            "activity_events",
            "conversation_fields",
            "conversation_tags",
            "conversations",
            "copilot_messages",
            "copilot_sessions",
            "custom_object_fields",
            "custom_object_links",
            "custom_object_types",
            "custom_objects",
            "customer_addresses",
            "customer_emails",
            "customer_events",
            "customer_memory",
            "customer_phones",
            "customer_properties",
            "customer_property_definitions",
            "customer_social_profiles",
            "customer_websites",
            "customers",
            "daily_metrics",
            "do_not_contact",
            "docs_articles",
            "docs_categories",
            "docs_chunks",
            "docs_collections",
            "encrypted_sync_log",
            "folders",
            "friction_findings",
            "inbox_field_options",
            "inbox_fields",
            "inbox_views",
            "incident_conversations",
            "incident_events",
            "incident_notes",
            "incident_refs",
            "incident_related",
            "incident_releases",
            "incidents",
            "issue_cluster_conversations",
            "issue_clusters",
            "jobs",
            "knowledge_gap_candidates",
            "knowledge_chunks",
            "knowledge_doc_usage",
            "knowledge_documents",
            "knowledge_sources",
            "known_issue_conversations",
            "known_issue_refs",
            "known_issues",
            "mailbox_business_hours",
            "mailboxes",
            "metric_definitions",
            "notification_prefs",
            "notifications",
            "oauth_tokens",
            "organization_properties",
            "organization_property_definitions",
            "organizations",
            "outbound_attempts",
            "outbound_jobs",
            "outreach_attempts",
            "outreach_campaigns",
            "outreach_events",
            "outreach_recipients",
            "post_resolution_qa",
            "products",
            "ratings",
            "release_events",
            "report_definitions",
            "report_snapshots",
            "routing_configurations",
            "saved_replies",
            "secrets",
            "saved_segments",
            "side_thread_mentions",
            "side_thread_messages",
            "side_thread_participants",
            "side_threads",
            "support_cases",
            "graph_edges",
            "sync_checkpoints",
            "sync_cursors",
            "sync_runs",
            "system_users",
            "tags",
            "team_members",
            "teams",
            "thread_participants",
            "thread_recipients",
            "conversation_threads",
            "ticket_state_transitions",
            "ticket_states",
            "translation_cache",
            "user_statuses",
            "users",
            "webhook_configs",
            "webhook_events",
            "workflows",
            // Runtime-ensured by the reference (aiRepo) — part of the
            // port's boot chain since AI-23.
            "golden_test_set",
        ];
        assert_eq!(
            reference.len(),
            124,
            "123 migration tables + golden_test_set"
        );
        for t in reference {
            assert!(
                table_exists(&conn, t).unwrap(),
                "reference table {t} missing after boot"
            );
        }
        // The legacy names are gone.
        assert!(!table_exists(&conn, "docs").unwrap());
        assert!(!table_exists(&conn, "issue_cluster_members").unwrap());
    }

    #[test]
    fn m044_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        apply_m044(&conn).unwrap();
        apply_m044(&conn).unwrap();
    }

    #[test]
    fn m044_renames_docs_preserving_data_and_columns() {
        // Simulate a pre-M044 database: rename the booted table back to the
        // legacy name, seed it, forget version 35, then re-boot — M044 must
        // adopt the data in place.
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn.execute_batch(
            "ALTER TABLE docs_articles RENAME TO docs;
             DELETE FROM _migrations WHERE version >= 35;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO docs_collections (id, name) VALUES (1, 'Manual')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO docs (remote_id, collection_local_id, name, text, status, preview, words)
             VALUES (9101, 1, 'Billing basics', 'How billing works', 'published', 'billing…', 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO docs (remote_id, collection_local_id, name, number, slug)
             VALUES (9102, 1, 'Refunds', 42, 'refunds')",
            [],
        )
        .unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        let (name, preview, words, number): (String, Option<String>, Option<i64>, Option<i64>) =
            conn.query_row(
                "SELECT name, preview, words, number FROM docs_articles WHERE remote_id = 9101",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(name, "Billing basics");
        assert_eq!(preview.as_deref(), Some("billing…"));
        assert_eq!(words, Some(2));
        assert_eq!(number, None);
        let n2: i64 = conn
            .query_row(
                "SELECT number FROM docs_articles WHERE remote_id = 9102",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n2, 42);
    }

    #[test]
    fn m044_converges_issue_cluster_links_to_the_reference_shape() {
        // Simulate the pre-M044 legacy shape (M016: no conversation_id FK),
        // then re-boot — M044 rebuilds it reference-exact.
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn.execute_batch(
            "DROP TABLE issue_cluster_conversations;
             DELETE FROM _migrations WHERE version >= 35;
             CREATE TABLE issue_cluster_members (
                 cluster_id      INTEGER NOT NULL
                     REFERENCES issue_clusters (id) ON DELETE CASCADE,
                 conversation_id INTEGER NOT NULL,
                 assigned_at     TEXT NOT NULL DEFAULT (datetime('now')),
                 PRIMARY KEY (cluster_id, conversation_id)
             );",
        )
        .unwrap();
        // A cluster with a live link and an orphaned link.
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (501, 501, 'active', 1, 1)",
            [],
        )
        .unwrap();
        let conv: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 501",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute("INSERT INTO issue_clusters (name) VALUES ('billing')", [])
            .unwrap();
        let cluster: i64 = conn
            .query_row(
                "SELECT id FROM issue_clusters WHERE name = 'billing'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO issue_cluster_members (cluster_id, conversation_id) VALUES (?1, ?2)",
            [cluster, conv],
        )
        .unwrap();
        // Orphan: conversation 99999 never existed (what CASCADE would
        // have removed had the FK existed).
        conn.execute(
            "INSERT INTO issue_cluster_members (cluster_id, conversation_id) VALUES (?1, 99999)",
            [cluster],
        )
        .unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        let (n, assigned): (i64, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*), MIN(assigned_at) FROM issue_cluster_conversations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "the orphaned link converges away");
        assert!(assigned.is_some(), "assigned_at carried over");
        // The reference FK is now enforced.
        let err = conn.execute(
            "INSERT INTO issue_cluster_conversations (cluster_id, conversation_id)
             VALUES (?1, 424242)",
            [cluster],
        );
        assert!(err.is_err(), "FK on conversation_id must be enforced");
    }

    #[test]
    fn m044_ensures_encrypted_sync_log_in_the_boot_chain() {
        let conn = booted();
        assert!(table_exists(&conn, "encrypted_sync_log").unwrap());
        // Reference DDL shape (migration 009).
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(encrypted_sync_log)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .collect();
        assert_eq!(
            cols,
            vec![
                "id",
                "direction",
                "file_path",
                "size_bytes",
                "conversations",
                "customers",
                "sha256",
                "at"
            ]
        );
    }

    /// The columns of a table via PRAGMA table_info.
    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        conn.prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .collect()
    }

    #[test]
    fn m045_restores_the_reference_actor_model() {
        let conn = booted();
        let cols = columns(&conn, "conversation_threads");
        // The reference column names are in, the collapsed ones are gone.
        for c in [
            "type",
            "body_text",
            "from_type",
            "created_by_user_id",
            "created_by_customer_id",
            "created_by_system_user_id",
            "scheduled_for",
        ] {
            assert!(cols.iter().any(|x| x == c), "column {c} missing");
        }
        for gone in ["thread_type", "body", "actor_type", "actor_id"] {
            assert!(
                !cols.iter().any(|x| x == gone),
                "column {gone} must be gone"
            );
        }
        // The port's additive extras stay.
        for extra in ["created_at", "remote_id", "fts_indexed", "embedding_state"] {
            assert!(
                cols.iter().any(|x| x == extra),
                "extra column {extra} dropped"
            );
        }

        // FK enforcement: a thread on a nonexistent conversation is refused.
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (501, 501, 'active', 1, 1)",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type)
                 VALUES (424242, 'reply', 'x', 'user')",
                [],
            )
            .is_err());
        // CASCADE: deleting the conversation removes its threads.
        let conv: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 501",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type)
             VALUES (?1, 'reply', 'hello', 'user')",
            [conv],
        )
        .unwrap();
        conn.execute("DELETE FROM conversations WHERE id = ?1", [conv])
            .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_threads WHERE conversation_id = ?1",
                [conv],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "threads cascade away with their conversation");

        // The actor FKs: a user id that does not exist is refused.
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (502, 502, 'active', 1, 1)",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO conversation_threads
                     (conversation_id, type, body_text, from_type, created_by_user_id)
                 SELECT 1, 'reply', 'x', 'user', 424242
                  FROM conversations LIMIT 1",
                [],
            )
            .is_err());
    }

    /// The (from, table, to) FK set of a table.
    fn fks(conn: &Connection, table: &str) -> Vec<(String, String, String)> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA foreign_key_list({table})"))
            .unwrap();
        let mut out: Vec<(String, String, String)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn m045_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        apply_m045(&conn).unwrap();
        apply_m045(&conn).unwrap();
    }

    #[test]
    fn m045_adopts_pre_m045_rows_in_place() {
        // Simulate a pre-M045 database: rebuild the collapsed shape, seed it
        // (a valid user actor, a valid customer actor, a 'system' line with
        // id 0, a user actor that resolves to nothing, an orphaned thread),
        // forget version 36, then re-boot — M045 must adopt the data in
        // place with the converging copy.
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn.execute_batch(
            "DROP TABLE conversation_threads;
             DELETE FROM _migrations WHERE version >= 36;
             CREATE TABLE conversation_threads (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER NOT NULL,
                remote_id       INTEGER,
                thread_type     TEXT NOT NULL,
                state           TEXT DEFAULT 'published',
                body            TEXT,
                body_html       TEXT,
                from_name       TEXT,
                from_email      TEXT,
                actor_type      TEXT NOT NULL,
                actor_id        INTEGER,
                assigned_to_type TEXT,
                assigned_to_id  INTEGER,
                saved_reply_local_id INTEGER,
                action_type     TEXT,
                action_text     TEXT,
                to_list         TEXT,
                cc_list         TEXT,
                bcc_list        TEXT,
                scheduled_for   TEXT,
                remote_created_at TEXT,
                remote_updated_at TEXT,
                local_created_at TEXT NOT NULL DEFAULT (datetime('now')),
                local_updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_synced_at  TEXT,
                raw_json        TEXT,
                raw_json_hash   TEXT,
                deleted_at      TEXT,
                fts_indexed     INTEGER DEFAULT 0,
                embedding_state TEXT DEFAULT 'not_indexed',
                created_at      TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (501, 501, 'active', 1, 1)",
            [],
        )
        .unwrap();
        let conv: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 501",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO users (id, remote_id, first_name) VALUES (2, 2002, 'Grace')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO customers (id, remote_id, first_name) VALUES (10, 3001, 'Ada')",
            [],
        )
        .unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO conversation_threads
                (conversation_id, thread_type, body, actor_type, actor_id, created_at)
             VALUES
                ({conv}, 'reply',    'We are on it', 'user',     2,    '2026-01-01T10:05:00Z'),
                ({conv}, 'customer', 'I want a refund', 'customer', 10, '2026-01-01T10:00:00Z'),
                ({conv}, 'lineitem', 'status changed', 'system',  0,    '2026-01-01T10:06:00Z'),
                ({conv}, 'reply',    'ghost actor',   'user',     999, '2026-01-01T10:07:00Z'),
                (424242, 'reply',    'orphan',        'user',     2,    '2026-01-01T10:08:00Z');"
        ))
        .unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();

        // The valid actors land in their 3-way columns; the ghost actor and
        // the system 0 converge to NULL; the orphan converges away.
        let rows: Vec<(String, String, Option<i64>, Option<i64>, Option<i64>)> = conn
            .prepare(
                "SELECT body_text, from_type, created_by_user_id,
                        created_by_customer_id, created_by_system_user_id
                   FROM conversation_threads ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(rows.len(), 4, "the orphaned thread converges away");
        assert_eq!(
            rows[0],
            ("We are on it".into(), "user".into(), Some(2), None, None)
        );
        assert_eq!(
            rows[1],
            (
                "I want a refund".into(),
                "customer".into(),
                None,
                Some(10),
                None
            )
        );
        // 'system' converges to the reference 'system_user' vocabulary.
        assert_eq!(
            rows[2],
            (
                "status changed".into(),
                "system_user".into(),
                None,
                None,
                None
            )
        );
        // The ghost user id resolves to NULL (FK-enforced).
        assert_eq!(
            rows[3],
            ("ghost actor".into(), "user".into(), None, None, None)
        );
    }
}
