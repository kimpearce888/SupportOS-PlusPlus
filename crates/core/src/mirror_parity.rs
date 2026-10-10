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
//! * **M046 (DB-06)** — foreign keys on the base mirror tables
//!   (customers, attachments, activity_events, notifications,
//!   side_threads, knowledge_gap_candidates, customer_memory,
//!   issue_clusters, ai_runs, outreach_campaigns, state_transitions),
//!   matching the reference's enforcement.
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

/// M046 — DB-06: foreign keys on the base mirror tables, matching the
/// reference's enforcement (001/002/003/009/011/012/015).
///
/// The port created these tables without their reference FK clauses
/// (guard-less `CREATE TABLE`s from the pre-parity batches); M046
/// rebuilds each one with the port's own column set plus the reference
/// FK set. The converging copy enforces the invariants the reference
/// always had: orphaned CASCADE rows drop, invalid SET-NULL-able
/// references become NULL. Column names stay the port's documented ones
/// (`notifications.target_user_id`, `customer_memory`'s port shape,
/// `ticket_state_transitions`'s remote keying) — column parity is not
/// DB-06's scope.
///
/// Pragma parity (WAL, `foreign_keys=ON`, `busy_timeout`) is enforced by
/// [`crate::db::open`] — verified by test.
pub fn apply_m046(conn: &Connection) -> Result<()> {
    // customers: organization_id -> organizations (001: SET NULL).
    if !fk_declared(conn, "customers", "organization_id")? {
        rebuild_table(
            conn,
            "customers",
            &Rebuild {
                new_name: "customers",
                new_ddl: "(
                    id               INTEGER PRIMARY KEY,
                    remote_id        INTEGER NOT NULL UNIQUE,
                    first_name       TEXT,
                    last_name        TEXT,
                    email            TEXT,
                    organization     TEXT,
                    job_title        TEXT,
                    phone            TEXT,
                    created_at       TEXT,
                    updated_at       TEXT,
                    local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    deleted_at       TEXT,
                    organization_id  INTEGER REFERENCES organizations (id) ON DELETE SET NULL,
                    photo_url        TEXT,
                    raw_json         TEXT,
                    raw_json_hash    TEXT,
                    remote_created_at TEXT,
                    remote_updated_at TEXT,
                    last_seen_at     TEXT,
                    last_synced_at   TEXT,
                    local_updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    background       TEXT,
                    age              TEXT,
                    gender           TEXT,
                    location         TEXT
                )",
                copy_sql: "INSERT INTO \"customers__rebuild\"
                               (id, remote_id, first_name, last_name, email, organization,
                                job_title, phone, created_at, updated_at, local_created_at,
                                deleted_at, organization_id, photo_url, raw_json,
                                raw_json_hash, remote_created_at, remote_updated_at,
                                last_seen_at, last_synced_at, local_updated_at,
                                background, age, gender, location)
                           SELECT id, remote_id, first_name, last_name, email, organization,
                                  job_title, phone, created_at, updated_at, local_created_at,
                                  deleted_at,
                                  CASE WHEN organization_id IN (SELECT id FROM organizations)
                                       THEN organization_id END,
                                  photo_url, raw_json, raw_json_hash, remote_created_at,
                                  remote_updated_at, last_seen_at, last_synced_at,
                                  local_updated_at, background, age, gender, location
                             FROM customers",
                replacement_indexes: &[],
            },
        )?;
    }

    // attachments: thread/conversation CASCADEs (001).
    if !fk_declared(conn, "attachments", "thread_id")? {
        rebuild_table(
            conn,
            "attachments",
            &Rebuild {
                new_name: "attachments",
                new_ddl: "(
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    remote_id       INTEGER UNIQUE,
                    thread_id       INTEGER NOT NULL
                                        REFERENCES conversation_threads (id) ON DELETE CASCADE,
                    conversation_id INTEGER NOT NULL
                                        REFERENCES conversations (id) ON DELETE CASCADE,
                    filename        TEXT,
                    mime_type       TEXT,
                    size            INTEGER,
                    local_path       TEXT,
                    hash            TEXT,
                    downloaded_at   TEXT,
                    state           TEXT DEFAULT 'metadata',
                    raw_json        TEXT
                )",
                copy_sql: "INSERT INTO \"attachments__rebuild\"
                               (id, remote_id, thread_id, conversation_id, filename,
                                mime_type, size, local_path, hash, downloaded_at,
                                state, raw_json)
                           SELECT a.id, a.remote_id, a.thread_id, a.conversation_id,
                                  a.filename, a.mime_type, a.size, a.local_path, a.hash,
                                  a.downloaded_at, a.state, a.raw_json
                             FROM attachments a
                            WHERE a.conversation_id IN (SELECT id FROM conversations)
                              AND a.thread_id IN (SELECT id FROM conversation_threads)",
                replacement_indexes: &[],
            },
        )?;
    }

    // activity_events (the conversation_events mirror): conversation
    // CASCADE + thread SET NULL (011).
    if !fk_declared(conn, "activity_events", "thread_local_id")? {
        rebuild_table(
            conn,
            "activity_events",
            &Rebuild {
                new_name: "activity_events",
                new_ddl: "(
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_id INTEGER NOT NULL
                                        REFERENCES conversations (id) ON DELETE CASCADE,
                    event_type      TEXT NOT NULL,
                    actor_type      TEXT NOT NULL,
                    actor_id        INTEGER,
                    occurred_at     TEXT NOT NULL,
                    dedup_key        TEXT NOT NULL UNIQUE,
                    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    thread_local_id INTEGER REFERENCES conversation_threads (id) ON DELETE SET NULL,
                    source          TEXT NOT NULL DEFAULT 'sync',
                    metadata        TEXT
                )",
                copy_sql: "INSERT INTO \"activity_events__rebuild\"
                               (id, conversation_id, event_type, actor_type, actor_id,
                                occurred_at, dedup_key, created_at, thread_local_id,
                                source, metadata)
                           SELECT e.id, e.conversation_id, e.event_type, e.actor_type,
                                  e.actor_id, e.occurred_at, e.dedup_key, e.created_at,
                                  CASE WHEN e.thread_local_id IN (SELECT id FROM conversation_threads)
                                       THEN e.thread_local_id END,
                                  e.source, e.metadata
                             FROM activity_events e
                            WHERE e.conversation_id IN (SELECT id FROM conversations)",
                replacement_indexes: &[],
            },
        )?;
    }

    // notifications: target CASCADE + actor/conversation/customer FKs (012).
    if !fk_declared(conn, "notifications", "conversation_id")? {
        rebuild_table(
            conn,
            "notifications",
            &Rebuild {
                new_name: "notifications",
                new_ddl: "(
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    type            TEXT NOT NULL,
                    severity        TEXT NOT NULL DEFAULT 'info',
                    title           TEXT NOT NULL DEFAULT '',
                    body            TEXT,
                    target_user_id  INTEGER REFERENCES users (id) ON DELETE CASCADE,
                    actor_user_local_id INTEGER REFERENCES users (id) ON DELETE SET NULL,
                    conversation_id INTEGER REFERENCES conversations (id) ON DELETE CASCADE,
                    conversation_number INTEGER,
                    customer_local_id   INTEGER REFERENCES customers (id) ON DELETE SET NULL,
                    issue_id        INTEGER,
                    campaign_id     INTEGER,
                    job_id          INTEGER,
                    side_thread_id  INTEGER,
                    dedup_key        TEXT NOT NULL DEFAULT '',
                    payload         TEXT,
                    read_at         TEXT,
                    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
                )",
                copy_sql: "INSERT INTO \"notifications__rebuild\"
                               (id, type, severity, title, body, target_user_id,
                                actor_user_local_id, conversation_id, conversation_number,
                                customer_local_id, issue_id, campaign_id, job_id,
                                side_thread_id, dedup_key, payload, read_at, created_at)
                           SELECT n.id, n.type, n.severity, n.title, n.body,
                                  n.target_user_id,
                                  CASE WHEN n.actor_user_local_id IN (SELECT id FROM users)
                                       THEN n.actor_user_local_id END,
                                  n.conversation_id, n.conversation_number,
                                  CASE WHEN n.customer_local_id IN (SELECT id FROM customers)
                                       THEN n.customer_local_id END,
                                  n.issue_id, n.campaign_id, n.job_id, n.side_thread_id,
                                  n.dedup_key, n.payload, n.read_at, n.created_at
                             FROM notifications n
                            WHERE (n.conversation_id IS NULL
                                   OR n.conversation_id IN (SELECT id FROM conversations))
                              AND (n.target_user_id IS NULL
                                   OR n.target_user_id IN (SELECT id FROM users))",
                replacement_indexes: &[],
            },
        )?;
    }

    // side_threads: conversation CASCADE + team/creator SET NULL (012).
    if !fk_declared(conn, "side_threads", "conversation_id")? {
        rebuild_table(
            conn,
            "side_threads",
            &Rebuild {
                new_name: "side_threads",
                new_ddl: "(
                    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_id     INTEGER NOT NULL
                                            REFERENCES conversations (id) ON DELETE CASCADE,
                    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    created_by_user_id   INTEGER REFERENCES users (id) ON DELETE SET NULL,
                    title               TEXT NOT NULL DEFAULT '',
                    team_local_id       INTEGER REFERENCES teams (id) ON DELETE SET NULL,
                    status              TEXT NOT NULL DEFAULT 'open',
                    updated_at          TEXT NOT NULL DEFAULT (datetime('now')),
                    resolved_at         TEXT
                )",
                copy_sql: "INSERT INTO \"side_threads__rebuild\"
                               (id, conversation_id, created_at, created_by_user_id,
                                title, team_local_id, status, updated_at, resolved_at)
                           SELECT s.id, s.conversation_id, s.created_at,
                                  CASE WHEN s.created_by_user_id IN (SELECT id FROM users)
                                       THEN s.created_by_user_id END,
                                  s.title,
                                  CASE WHEN s.team_local_id IN (SELECT id FROM teams)
                                       THEN s.team_local_id END,
                                  s.status, s.updated_at, s.resolved_at
                             FROM side_threads s
                            WHERE s.conversation_id IN (SELECT id FROM conversations)",
                replacement_indexes: &[],
            },
        )?;
    }

    // knowledge_gap_candidates (the knowledge_candidates mirror): the
    // decider FK (015).
    if !fk_declared(conn, "knowledge_gap_candidates", "decided_by_user_local_id")? {
        rebuild_table(
            conn,
            "knowledge_gap_candidates",
            &Rebuild {
                new_name: "knowledge_gap_candidates",
                new_ddl: "(
                    id               INTEGER PRIMARY KEY AUTOINCREMENT,
                    query_text       TEXT NOT NULL,
                    occurrence_count INTEGER NOT NULL DEFAULT 1,
                    kind             TEXT,
                    status           TEXT NOT NULL DEFAULT 'open'
                                         CHECK (status IN ('open','approved','rejected')),
                    decision_note    TEXT,
                    decided_at       TEXT,
                    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    dedup_key        TEXT,
                    evidence_conversation_ids TEXT NOT NULL DEFAULT '[]',
                    related_document_ids TEXT NOT NULL DEFAULT '[]',
                    detail           TEXT,
                    decided_by_user_local_id INTEGER REFERENCES users (id) ON DELETE SET NULL,
                    updated_at       TEXT NOT NULL DEFAULT (datetime('now')),
                    provenance       TEXT NOT NULL DEFAULT 'deterministic_local'
                )",
                copy_sql: "INSERT INTO \"knowledge_gap_candidates__rebuild\"
                               (id, query_text, occurrence_count, kind, status,
                                decision_note, decided_at, created_at, dedup_key,
                                evidence_conversation_ids, related_document_ids,
                                detail, decided_by_user_local_id, updated_at, provenance)
                           SELECT k.id, k.query_text, k.occurrence_count, k.kind, k.status,
                                  k.decision_note, k.decided_at, k.created_at, k.dedup_key,
                                  k.evidence_conversation_ids, k.related_document_ids,
                                  k.detail,
                                  CASE WHEN k.decided_by_user_local_id IN (SELECT id FROM users)
                                       THEN k.decided_by_user_local_id END,
                                  k.updated_at, k.provenance
                             FROM knowledge_gap_candidates k",
                replacement_indexes: &[],
            },
        )?;
    }

    // customer_memory (the customer_memories mirror): customer CASCADE +
    // origin conversation SET NULL (003).
    if !fk_declared(conn, "customer_memory", "customer_id")? {
        rebuild_table(
            conn,
            "customer_memory",
            &Rebuild {
                new_name: "customer_memory",
                new_ddl: "(
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    customer_id     INTEGER NOT NULL
                                        REFERENCES customers (id) ON DELETE CASCADE,
                    memory_key      TEXT NOT NULL,
                    memory_value    TEXT NOT NULL,
                    evidence_excerpt TEXT NOT NULL,
                    source_conversation_id INTEGER
                                        REFERENCES conversations (id) ON DELETE SET NULL,
                    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    source          TEXT NOT NULL DEFAULT 'ai',
                    origin          TEXT DEFAULT 'conversation',
                    first_seen_at   TEXT,
                    last_seen_at    TEXT,
                    confidence      TEXT DEFAULT 'unknown',
                    provenance      TEXT DEFAULT 'ai_generated',
                    kind            TEXT NOT NULL DEFAULT 'fact'
                )",
                copy_sql: "INSERT INTO \"customer_memory__rebuild\"
                               (id, customer_id, memory_key, memory_value, evidence_excerpt,
                                source_conversation_id, created_at, source, origin,
                                first_seen_at, last_seen_at, confidence, provenance, kind)
                           SELECT m.id, m.customer_id, m.memory_key, m.memory_value,
                                  m.evidence_excerpt,
                                  CASE WHEN m.source_conversation_id
                                            IN (SELECT id FROM conversations)
                                       THEN m.source_conversation_id END,
                                  m.created_at, m.source, m.origin, m.first_seen_at,
                                  m.last_seen_at, m.confidence, m.provenance, m.kind
                             FROM customer_memory m
                            WHERE m.customer_id IN (SELECT id FROM customers)",
                replacement_indexes: &[],
            },
        )?;
    }

    // issue_clusters: the known-issue link SET NULLs (003).
    if !fk_declared(conn, "issue_clusters", "known_issue_id")? {
        rebuild_table(
            conn,
            "issue_clusters",
            &Rebuild {
                new_name: "issue_clusters",
                new_ddl: "(
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    name            TEXT NOT NULL,
                    conversation_count INTEGER NOT NULL DEFAULT 0,
                    first_seen_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    last_seen_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    status          TEXT NOT NULL DEFAULT 'active',
                    trend           TEXT NOT NULL DEFAULT 'stable',
                    known_issue_id  INTEGER REFERENCES known_issues (id) ON DELETE SET NULL,
                    product         TEXT,
                    title           TEXT,
                    summary         TEXT,
                    category        TEXT,
                    feature         TEXT,
                    ai_generated    INTEGER DEFAULT 0,
                    customer_count  INTEGER DEFAULT 0,
                    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
                    provenance      TEXT DEFAULT 'ai_generated'
                )",
                copy_sql: "INSERT INTO \"issue_clusters__rebuild\"
                               (id, name, conversation_count, first_seen_at, last_seen_at,
                                status, trend, known_issue_id, product, title, summary,
                                category, feature, ai_generated, customer_count,
                                created_at, updated_at, provenance)
                           SELECT i.id, i.name, i.conversation_count, i.first_seen_at,
                                  i.last_seen_at, i.status, i.trend,
                                  CASE WHEN i.known_issue_id IN (SELECT id FROM known_issues)
                                       THEN i.known_issue_id END,
                                  i.product, i.title, i.summary, i.category, i.feature,
                                  i.ai_generated, i.customer_count, i.created_at,
                                  i.updated_at, i.provenance
                             FROM issue_clusters i",
                replacement_indexes: &[],
            },
        )?;
    }

    // ai_runs: the conversation SET NULL (003).
    if !fk_declared(conn, "ai_runs", "conversation_id")? {
        rebuild_table(
            conn,
            "ai_runs",
            &Rebuild {
                new_name: "ai_runs",
                new_ddl: "(
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    input_hash      TEXT NOT NULL,
                    prompt_version  TEXT NOT NULL,
                    model           TEXT NOT NULL,
                    response_json   TEXT NOT NULL,
                    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    type            TEXT NOT NULL DEFAULT 'analysis',
                    conversation_id INTEGER REFERENCES conversations (id) ON DELETE SET NULL,
                    status          TEXT NOT NULL DEFAULT 'queued',
                    input_refs      TEXT,
                    error           TEXT,
                    latency_ms      INTEGER,
                    token_usage     TEXT,
                    started_at      TEXT,
                    completed_at    TEXT,
                    provenance      TEXT NOT NULL DEFAULT 'ai_generated'
                )",
                copy_sql: "INSERT INTO \"ai_runs__rebuild\"
                               (id, input_hash, prompt_version, model, response_json,
                                created_at, type, conversation_id, status, input_refs,
                                error, latency_ms, token_usage, started_at, completed_at,
                                provenance)
                           SELECT r.id, r.input_hash, r.prompt_version, r.model,
                                  r.response_json, r.created_at, r.type,
                                  CASE WHEN r.conversation_id IN (SELECT id FROM conversations)
                                       THEN r.conversation_id END,
                                  r.status, r.input_refs, r.error, r.latency_ms,
                                  r.token_usage, r.started_at, r.completed_at,
                                  r.provenance
                             FROM ai_runs r",
                replacement_indexes: &[],
            },
        )?;
    }

    // outreach_campaigns: mailbox + segment FKs (009; segments is the
    // port's saved_segments rename).
    if !fk_declared(conn, "outreach_campaigns", "mailbox_local_id")? {
        rebuild_table(
            conn,
            "outreach_campaigns",
            &Rebuild {
                new_name: "outreach_campaigns",
                new_ddl: "(
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    name            TEXT NOT NULL,
                    subject         TEXT NOT NULL,
                    body            TEXT NOT NULL,
                    mailbox_local_id INTEGER REFERENCES mailboxes (id),
                    tags            TEXT NOT NULL DEFAULT '[]',
                    status          TEXT NOT NULL DEFAULT 'draft',
                    segment_id      INTEGER REFERENCES saved_segments (id),
                    segment_snapshot TEXT,
                    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
                    queued_at       TEXT,
                    completed_at    TEXT,
                    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
                )",
                copy_sql: "INSERT INTO \"outreach_campaigns__rebuild\"
                               (id, name, subject, body, mailbox_local_id, tags, status,
                                segment_id, segment_snapshot, created_at, queued_at,
                                completed_at, updated_at)
                           SELECT o.id, o.name, o.subject, o.body,
                                  CASE WHEN o.mailbox_local_id IN (SELECT id FROM mailboxes)
                                       THEN o.mailbox_local_id END,
                                  o.tags, o.status,
                                  CASE WHEN o.segment_id IN (SELECT id FROM saved_segments)
                                       THEN o.segment_id END,
                                  o.segment_snapshot, o.created_at, o.queued_at,
                                  o.completed_at, o.updated_at
                             FROM outreach_campaigns o",
                replacement_indexes: &[],
            },
        )?;
    }

    // ticket_state_transitions: the port keys by the conversation's REMOTE
    // id (a documented divergence; the reference's local-id column set
    // does not exist here) — the FK rides the port's keying through
    // conversations(remote_id), preserving the reference's cascade
    // semantics (a deleted conversation takes its transitions with it).
    if !fk_declared(conn, "ticket_state_transitions", "conversation_remote_id")? {
        rebuild_table(
            conn,
            "ticket_state_transitions",
            &Rebuild {
                new_name: "ticket_state_transitions",
                new_ddl: "(
                    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_remote_id INTEGER NOT NULL
                                               REFERENCES conversations (remote_id) ON DELETE CASCADE,
                    from_state             TEXT,
                    to_state               TEXT,
                    from_priority          TEXT,
                    to_priority            TEXT,
                    actor_type             TEXT NOT NULL,
                    actor_id               INTEGER,
                    transitioned_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                )",
                copy_sql: "INSERT INTO \"ticket_state_transitions__rebuild\"
                               (id, conversation_remote_id, from_state, to_state,
                                from_priority, to_priority, actor_type, actor_id,
                                transitioned_at)
                           SELECT t.id, t.conversation_remote_id, t.from_state, t.to_state,
                                  t.from_priority, t.to_priority, t.actor_type, t.actor_id,
                                  t.transitioned_at
                             FROM ticket_state_transitions t
                            WHERE t.conversation_remote_id IN (SELECT remote_id FROM conversations)",
                replacement_indexes: &[],
            },
        )?;
    }

    let _ = conn.execute("UPDATE app_state SET schema_version = 46 WHERE id = 1", []);
    Ok(())
}

/// Does `table`.`column` already declare an outgoing FK (the idempotence
/// probe for M046 — a rebuilt table declares all of its reference FKs)?
fn fk_declared(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA foreign_key_list({table})"))?;
    let hits = stmt
        .query_map([], |r| r.get::<_, String>(3))?
        .filter_map(|r| r.ok())
        .filter(|from| from == column)
        .count();
    Ok(hits > 0)
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
    fn m046_declares_the_reference_fks() {
        let conn = booted();
        // customers -> organizations (SET NULL).
        assert!(fks(&conn, "customers").contains(&(
            "organization_id".into(),
            "organizations".into(),
            "id".into()
        )));
        // attachments -> conversation_threads + conversations (CASCADE).
        assert!(fks(&conn, "attachments").contains(&(
            "thread_id".into(),
            "conversation_threads".into(),
            "id".into()
        )));
        assert!(fks(&conn, "attachments").contains(&(
            "conversation_id".into(),
            "conversations".into(),
            "id".into()
        )));
        // activity_events -> conversations (CASCADE) + threads (SET NULL).
        assert!(fks(&conn, "activity_events").contains(&(
            "conversation_id".into(),
            "conversations".into(),
            "id".into()
        )));
        assert!(fks(&conn, "activity_events").contains(&(
            "thread_local_id".into(),
            "conversation_threads".into(),
            "id".into()
        )));
        // notifications -> users (target CASCADE, actor SET NULL),
        // conversations (CASCADE), customers (SET NULL).
        assert!(fks(&conn, "notifications").contains(&(
            "target_user_id".into(),
            "users".into(),
            "id".into()
        )));
        assert!(fks(&conn, "notifications").contains(&(
            "actor_user_local_id".into(),
            "users".into(),
            "id".into()
        )));
        assert!(fks(&conn, "notifications").contains(&(
            "conversation_id".into(),
            "conversations".into(),
            "id".into()
        )));
        assert!(fks(&conn, "notifications").contains(&(
            "customer_local_id".into(),
            "customers".into(),
            "id".into()
        )));
        // side_threads -> conversations (CASCADE), teams + users (SET NULL).
        assert!(fks(&conn, "side_threads").contains(&(
            "conversation_id".into(),
            "conversations".into(),
            "id".into()
        )));
        assert!(fks(&conn, "side_threads").contains(&(
            "team_local_id".into(),
            "teams".into(),
            "id".into()
        )));
        assert!(fks(&conn, "side_threads").contains(&(
            "created_by_user_id".into(),
            "users".into(),
            "id".into()
        )));
        // knowledge_gap_candidates -> users (SET NULL).
        assert!(fks(&conn, "knowledge_gap_candidates").contains(&(
            "decided_by_user_local_id".into(),
            "users".into(),
            "id".into()
        )));
        // customer_memory -> customers (CASCADE) + conversations (SET NULL).
        assert!(fks(&conn, "customer_memory").contains(&(
            "customer_id".into(),
            "customers".into(),
            "id".into()
        )));
        assert!(fks(&conn, "customer_memory").contains(&(
            "source_conversation_id".into(),
            "conversations".into(),
            "id".into()
        )));
        // issue_clusters -> known_issues (SET NULL).
        assert!(fks(&conn, "issue_clusters").contains(&(
            "known_issue_id".into(),
            "known_issues".into(),
            "id".into()
        )));
        // ai_runs -> conversations (SET NULL).
        assert!(fks(&conn, "ai_runs").contains(&(
            "conversation_id".into(),
            "conversations".into(),
            "id".into()
        )));
        // outreach_campaigns -> mailboxes + saved_segments.
        assert!(fks(&conn, "outreach_campaigns").contains(&(
            "mailbox_local_id".into(),
            "mailboxes".into(),
            "id".into()
        )));
        assert!(fks(&conn, "outreach_campaigns").contains(&(
            "segment_id".into(),
            "saved_segments".into(),
            "id".into()
        )));
        // ticket_state_transitions -> conversations(remote_id) (CASCADE,
        // the port's remote keying).
        assert!(fks(&conn, "ticket_state_transitions").contains(&(
            "conversation_remote_id".into(),
            "conversations".into(),
            "remote_id".into()
        )));

        // Enforcement: an activity event on a missing conversation is
        // refused; deleting a customer cascades its memories.
        assert!(conn
            .execute(
                "INSERT INTO activity_events
                     (conversation_id, event_type, actor_type, occurred_at, dedup_key)
                 VALUES (424242, 'x', 'user', 'now', 'db06-neg')",
                [],
            )
            .is_err());
        conn.execute(
            "INSERT INTO customers (remote_id, first_name) VALUES (701, 'Ada')",
            [],
        )
        .unwrap();
        let customer: i64 = conn
            .query_row("SELECT id FROM customers WHERE remote_id = 701", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO customer_memory
                 (customer_id, memory_key, memory_value, evidence_excerpt)
             VALUES (?1, 'pref', 'dark mode', 'she said so')",
            [customer],
        )
        .unwrap();
        conn.execute("DELETE FROM customers WHERE id = ?1", [customer])
            .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM customer_memory WHERE customer_id = ?1",
                [customer],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "memories cascade away with their customer");
    }

    #[test]
    fn m046_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        apply_m046(&conn).unwrap();
        apply_m046(&conn).unwrap();
    }

    /// DB-06 pragma parity: db::open enforces WAL, foreign_keys and a
    /// busy_timeout (verified on a real file connection, the production
    /// boot path).
    #[test]
    fn db06_pragma_parity_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("pragma.db")).unwrap();
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(timeout >= 1000, "busy_timeout set, got {timeout}");
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
