//! Local search engine on SQLite FTS5 — reference-parity port of
//! `src/server/search/searchEngine.ts` (spec #25).
//!
//! Keyword/full-text search across conversations, threads, customers,
//! knowledge, known issues, saved replies and AI analyses. Works WITHOUT a
//! vector db; semantic results are merged in by the hybrid layer when an
//! embedding model is available (`used_semantic` / `semantic_available` are
//! therefore always `false` from this engine, exactly like the reference).
//!
//! # FTS5 tables (reference-exact)
//!
//! [`apply_fts_migration`] creates all 8 reference virtual tables:
//!
//! | table                 | columns                                                              | source       |
//! |-----------------------|----------------------------------------------------------------------|--------------|
//! | `fts_conversations`   | subject, preview, customer, tags, numbers + conversation_id UNINDEXED | migration 004 |
//! | `fts_threads`         | body + thread_id/conversation_id UNINDEXED                            | migration 004 |
//! | `fts_knowledge`       | title, content + chunk_id/document_id/visibility UNINDEXED            | migration 004 |
//! | `fts_known_issues`    | title, symptoms, workaround, customer_safe_explanation + known_issue_id UNINDEXED | migration 004 |
//! | `fts_saved_replies`   | name, preview, text + saved_reply_id UNINDEXED                        | migration 004 |
//! | `fts_ai_analyses`     | summary, primary_question, intent + conversation_id/run_id UNINDEXED  | migration 004 |
//! | `docs_fts`            | name, text + article_id UNINDEXED                                     | migration 007 |
//! | `fts_custom_objects`  | title, search_text + object_id UNINDEXED                              | migration 014 |
//!
//! The legacy `conversations_fts` / `customers_fts` tables are still created
//! (and written by the legacy [`index_conversation`] / [`index_customer`]) so
//! pre-parity callers keep compiling and working.
//!
//! # Deviations from the reference (forced by port schema differences)
//!
//! - Column renames: `customer_local_id`, `mailbox_local_id` and
//!   `assignee_local_id` keep the reference names after DB-03/M047,
//!   `remote_created_at` → `created_at`, `last_activity_at` → `updated_at`,
//!   threads.`body_text` → `conversation_threads`.`body_text` (DB-04),
//!   `ct.tag_local_id` → `ct.tag_id`, `docs_articles` → `docs`,
//!   custom_objects.`properties` → `data_json` (search_text is rebuilt from
//!   the JSON values on rebuild).
//! - `apply_fts_migration` also ensures the base-table columns/tables the
//!   engine reads (soft-delete markers, `customer_emails`, knowledge/AI
//!   mirror tables) because the port attaches the FTS layer lazily instead
//!   of through a numbered migration.
//! - Search helpers return empty results (instead of erroring) when optional
//!   base tables (`known_issues`, `saved_replies`, `docs`, …) have not been
//!   created yet; the reference always has them after migrations.
//! - `SearchHit.score` serializes as an integer when whole (the reference
//!   emits `1`, not `1.0`).

use rusqlite::{params, Connection, OptionalExtension, ToSql};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Maximum search query length (legacy universal_search hardening).
pub const MAX_SEARCH_QUERY_LENGTH: usize = 500;

/// Maximum number of results per resource type (legacy bound).
pub const MAX_RESULTS_PER_TYPE: u32 = 50;

/// The reference `SearchScope` vocabulary (`shared/types.ts`).
pub const SEARCH_SCOPES: &[&str] = &[
    "all",
    "tickets",
    "customers",
    "knowledge",
    "issues",
    "saved_replies",
    "ai",
];

// ─── Types ────────────────────────────────────────────────────────────────

/// One search hit — reference `SearchHit` (`shared/types.ts`).
///
/// Field names serialize exactly like the reference JSON (`scope`, `id`,
/// `title`, `subtitle`, `snippet`, `score`, `href`, `why`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// Which collection the hit came from.
    pub scope: String,
    /// Local row id of the matched entity.
    pub id: i64,
    /// Primary display line.
    pub title: String,
    /// Secondary display line.
    pub subtitle: String,
    /// Matched text excerpt (FTS5 `snippet()` output where applicable).
    pub snippet: String,
    /// Relevance score (1 for keyword hits, 0.5 for the recent fallback).
    #[serde(serialize_with = "serialize_score")]
    pub score: f64,
    /// UI route to open when the hit is picked.
    pub href: String,
    /// Provenance notes ("keyword match", "recent", …).
    pub why: Vec<String>,
}

/// Serialize whole numbers as integers (`1`, not `1.0`) — the reference emits
/// `score: 1` for keyword hits and `score: 0.5` for the recent fallback.
fn serialize_score<S>(value: &f64, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.0e15 {
        serializer.serialize_i64(*value as i64)
    } else {
        serializer.serialize_f64(*value)
    }
}

/// Search filters — reference `SearchFilters` (`shared/types.ts`).
///
/// Mirrors JS truthiness: `Some(0)` values and `status = "all"` count as "no
/// filter" (the reference's `!!f.mailbox_id` etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilters {
    /// `all` (or absent) means no status filter.
    pub status: Option<String>,
    /// Mailbox local id (0 = no filter).
    pub mailbox_id: Option<i64>,
    /// Tag name (case-insensitive).
    pub tag: Option<String>,
    /// Only conversations created within this many days (0 = no filter).
    pub since_days: Option<i64>,
    /// Assignee local id (0 = no filter).
    pub assignee_id: Option<i64>,
}

/// Search response — reference `SearchResponse` (`shared/types.ts`).
///
/// `used_semantic` / `semantic_available` are always `false` from the engine;
/// the hybrid route layers semantics on top.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    /// The query that produced these hits.
    pub query: String,
    /// Hits across the requested scopes.
    pub hits: Vec<SearchHit>,
    /// `hits.len()` — kept as its own field like the reference.
    pub total: usize,
    /// Whether semantic retrieval contributed to these hits.
    pub used_semantic: bool,
    /// Whether semantic retrieval was possible at all.
    pub semantic_available: bool,
    /// Human-readable retrieval-mode note (set by the route, not the engine).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode_note: Option<String>,
}

/// Raw knowledge hit with document id + visibility (used by the evidence
/// builder + knowledge search API) — the return shape of the reference
/// `searchKnowledgeRaw`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRawHit {
    /// Owning knowledge document (local id).
    pub document_id: i64,
    /// Document title indexed with the chunk.
    pub title: String,
    /// FTS5 snippet with `[` `]` markers around matches.
    pub snippet: String,
    /// `customer_safe` or `internal_only`.
    pub visibility: String,
}

/// Legacy universal-search result (pre-parity IPC/UI shape). Kept so old
/// callers (`hybrid_search`, the knowledge route, the Tauri IPC command)
/// keep compiling; new callers should use [`search`] + [`SearchHit`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Scope name of the hit ("tickets", "customers", …).
    pub resource_type: String,
    /// Local row id of the matched entity.
    pub remote_id: i64,
    /// Primary display line.
    pub title: String,
    /// Matched text excerpt.
    pub snippet: String,
}

// ─── Schema ───────────────────────────────────────────────────────────────

/// Apply the FTS migration: create every reference FTS5 table (migrations
/// 004/007/014) plus the legacy pair, and ensure the base-table shapes the
/// engine reads exist. Idempotent (`IF NOT EXISTS` / PRAGMA-guarded ALTERs).
///
/// The base-table ensures are the port's substitute for the reference's
/// numbered migrations: the port attaches the search layer lazily (routes,
/// workers, IPC) rather than at boot, so the engine makes its own guarantees.
pub fn apply_fts_migration(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS fts_conversations USING fts5(
            subject, preview, customer, tags, numbers,
            conversation_id UNINDEXED
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS fts_threads USING fts5(
            body,
            thread_id UNINDEXED, conversation_id UNINDEXED
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS fts_knowledge USING fts5(
            title, content,
            chunk_id UNINDEXED, document_id UNINDEXED, visibility UNINDEXED
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS fts_known_issues USING fts5(
            title, symptoms, workaround, customer_safe_explanation,
            known_issue_id UNINDEXED
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS fts_saved_replies USING fts5(
            name, preview, text,
            saved_reply_id UNINDEXED
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS fts_ai_analyses USING fts5(
            summary, primary_question, intent,
            conversation_id UNINDEXED, run_id UNINDEXED
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
            name, text,
            article_id UNINDEXED
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS fts_custom_objects USING fts5(
            title,
            search_text,
            object_id UNINDEXED
        );

        -- Legacy pre-parity tables (index_conversation / index_customer).
        CREATE VIRTUAL TABLE IF NOT EXISTS conversations_fts
         USING fts5(remote_id UNINDEXED, subject, preview);
        CREATE VIRTUAL TABLE IF NOT EXISTS customers_fts
         USING fts5(remote_id UNINDEXED, first_name, last_name, email, organization);",
    )?;

    // Reference-shaped soft-delete markers + merge marker + FTS indexing flag
    // (the reference gets these from migration 001; the port adds them lazily
    // so the engine's SQL is valid on every database).
    if table_exists(conn, "conversations")? {
        add_column_if_missing(conn, "conversations", "deleted_at", "TEXT")?;
        add_column_if_missing(
            conn,
            "conversations",
            "merged_into_conversation_id",
            "INTEGER",
        )?;
    }
    if table_exists(conn, "customers")? {
        add_column_if_missing(conn, "customers", "deleted_at", "TEXT")?;
        add_column_if_missing(conn, "customers", "organization_id", "INTEGER")?;
    }
    if table_exists(conn, "conversation_threads")? {
        add_column_if_missing(conn, "conversation_threads", "deleted_at", "TEXT")?;
        add_column_if_missing(
            conn,
            "conversation_threads",
            "fts_indexed",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    if table_exists(conn, "known_issues")? {
        add_column_if_missing(conn, "known_issues", "title", "TEXT")?;
        add_column_if_missing(conn, "known_issues", "symptoms", "TEXT")?;
        add_column_if_missing(conn, "known_issues", "workaround", "TEXT")?;
        add_column_if_missing(conn, "known_issues", "customer_safe_explanation", "TEXT")?;
    }

    // Base tables the engine's SQL references directly. Same DDL as their
    // owning migrations (M039/M029/M030) — CREATE IF NOT EXISTS no-ops when
    // those have already run.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS knowledge_sources (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL,
            kind        TEXT DEFAULT 'local_file',
            visibility  TEXT NOT NULL DEFAULT 'internal_only',
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS knowledge_documents (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id       INTEGER NOT NULL REFERENCES knowledge_sources (id) ON DELETE CASCADE,
            title           TEXT NOT NULL,
            visibility      TEXT NOT NULL DEFAULT 'internal_only',
            version         INTEGER DEFAULT 1,
            checksum        TEXT,
            content         TEXT,
            format          TEXT DEFAULT 'markdown',
            last_reviewed_at TEXT,
            last_verified_at TEXT,
            last_indexed_at TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
            provenance      TEXT DEFAULT 'human_local'
        );
        CREATE INDEX IF NOT EXISTS idx_knowledge_documents_source
            ON knowledge_documents (source_id);

        CREATE TABLE IF NOT EXISTS knowledge_chunks (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            document_id     INTEGER NOT NULL REFERENCES knowledge_documents (id) ON DELETE CASCADE,
            chunk_index     INTEGER NOT NULL,
            content         TEXT NOT NULL,
            fts_indexed     INTEGER DEFAULT 0,
            embedding_state TEXT DEFAULT 'not_indexed',
            embedding_model TEXT,
            embedding       BLOB,
            chunk_version   INTEGER DEFAULT 2,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (document_id, chunk_index)
        );
        CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_doc ON knowledge_chunks (document_id);

        CREATE TABLE IF NOT EXISTS customer_emails (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id   INTEGER UNIQUE,
            customer_id INTEGER NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
            value       TEXT NOT NULL,
            type        TEXT,
            UNIQUE (customer_id, value)
        );
        CREATE INDEX IF NOT EXISTS idx_customer_emails_value ON customer_emails (value);

        CREATE TABLE IF NOT EXISTS ai_runs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            input_hash      TEXT NOT NULL,
            prompt_version  TEXT NOT NULL,
            model           TEXT NOT NULL,
            response_json   TEXT NOT NULL,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_ai_runs_lookup
            ON ai_runs (input_hash, prompt_version, model);

        CREATE TABLE IF NOT EXISTS ai_extracted_facts (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER REFERENCES conversations (id) ON DELETE CASCADE,
            run_id          INTEGER REFERENCES ai_runs (id) ON DELETE CASCADE,
            key             TEXT NOT NULL,
            value           TEXT,
            confidence      TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_ai_facts_conversation
            ON ai_extracted_facts (conversation_id);

        CREATE TABLE IF NOT EXISTS organizations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            name TEXT NOT NULL,
            domains TEXT,
            raw_json TEXT,
            raw_json_hash TEXT,
            remote_created_at TEXT,
            remote_updated_at TEXT,
            last_seen_at TEXT,
            last_synced_at TEXT,
            local_created_at TEXT NOT NULL DEFAULT (datetime('now')),
            local_updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            deleted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_organizations_name ON organizations(name);

        CREATE TABLE IF NOT EXISTS conversation_tags (
            conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            tag_id         INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (conversation_id, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_tags_tag
            ON conversation_tags(tag_id);",
    )?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
            params![name],
            |_| Ok(()),
        )
        .optional()?;
    Ok(exists.is_some())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let present = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|c| c.ok())
        .any(|c| c == column);
    Ok(present)
}

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

// ─── Tokenization ─────────────────────────────────────────────────────────

/// Build an FTS5 AND query from a user query — reference `ftsQuery`.
///
/// Strips `"*()`, splits on whitespace, keeps the first 8 non-empty tokens
/// and joins them as quoted prefix phrases: `"tok1"* "tok2"* …`.
/// Returns `""` (an empty phrase) when nothing survives.
#[must_use]
pub fn fts_query(q: &str) -> String {
    let tokens = tokenize(q, 1, 8);
    if tokens.is_empty() {
        return "\"\"".to_string();
    }
    tokens
        .iter()
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build an FTS5 OR query — reference `ftsQueryOr`.
///
/// Same stripping, but only tokens longer than 2 characters, capped at
/// `max_tokens` (default 10 in the reference), joined with ` OR `:
/// `"tok1"* OR "tok2"* …`. Used for similarity/evidence retrieval.
#[must_use]
pub fn fts_query_or(q: &str, max_tokens: usize) -> String {
    let tokens = tokenize(q, 3, max_tokens);
    if tokens.is_empty() {
        return "\"\"".to_string();
    }
    tokens
        .iter()
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Reference tokenization: replace `["*()]` with spaces, split on whitespace,
/// keep tokens whose char count is at least `min_chars`, cap at `max_tokens`.
pub(crate) fn tokenize(q: &str, min_chars: usize, max_tokens: usize) -> Vec<String> {
    let cleaned: String = q
        .chars()
        .map(|c| {
            if matches!(c, '"' | '*' | '(' | ')') {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .filter(|t| t.chars().count() >= min_chars)
        .take(max_tokens)
        .map(str::to_string)
        .collect()
}

/// Escape LIKE wildcards (`\`, `%`, `_`) — reference `searchCustomers`.
fn escape_like(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for c in q.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ─── Conversations ────────────────────────────────────────────────────────

/// Search tickets — reference `searchConversations`.
///
/// - Soft-deleted and merged conversations are always excluded.
/// - Filters: status (unless `all`), mailbox, tag (via
///   `conversation_tags` + `tags` by name, case-insensitive), since_days
///   (on `created_at`), assignee.
/// - A pure-digit query additionally filters `number = query`.
/// - With a non-empty FTS query, results join the best (MIN) rank over a
///   UNION ALL of `fts_conversations` + `fts_threads` + `fts_ai_analyses`,
///   ordered by rank then recency.
/// - Empty query + no filters returns recent conversations (score 0.5,
///   `why: ["recent"]`) ordered by `updated_at`.
pub fn search_conversations(
    conn: &Connection,
    query: &str,
    filters: &SearchFilters,
    limit: i64,
) -> Result<Vec<SearchHit>> {
    let fts = fts_query(query);
    if fts == "\"\"" && !has_filters(filters) {
        // No query, no filters: return recent conversations.
        return recent_conversations(conn, limit);
    }

    let mut where_sql = vec![
        "c.deleted_at IS NULL".to_string(),
        "c.merged_into_conversation_id IS NULL".to_string(),
    ];
    let mut args: Vec<Box<dyn ToSql>> = Vec::new();

    if status_applies(filters) {
        let idx = args.len() + 1;
        where_sql.push(format!("c.status = ?{idx}"));
        args.push(Box::new(filters.status.clone().unwrap_or_default()));
    }
    if let Some(mailbox) = filters.mailbox_id.filter(|v| *v != 0) {
        let idx = args.len() + 1;
        where_sql.push(format!("c.mailbox_local_id = ?{idx}"));
        args.push(Box::new(mailbox));
    }
    if let Some(tag) = filters.tag.clone().filter(|t| !t.is_empty()) {
        let idx = args.len() + 1;
        where_sql.push(format!(
            "EXISTS (SELECT 1 FROM conversation_tags ct JOIN tags t ON t.id = ct.tag_id \
             WHERE ct.conversation_id = c.id AND t.name = ?{idx} COLLATE NOCASE)"
        ));
        args.push(Box::new(tag));
    }
    if let Some(days) = filters.since_days.filter(|v| *v != 0) {
        let idx = args.len() + 1;
        where_sql.push(format!(
            "julianday(c.created_at) >= julianday('now', '-' || ?{idx} || ' days')"
        ));
        args.push(Box::new(days));
    }
    if let Some(assignee) = filters.assignee_id.filter(|v| *v != 0) {
        let idx = args.len() + 1;
        where_sql.push(format!("c.assignee_local_id = ?{idx}"));
        args.push(Box::new(assignee));
    }
    // Exact conversation number match wins.
    let trimmed = query.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(num) = trimmed.parse::<i64>() {
            let idx = args.len() + 1;
            where_sql.push(format!("c.number = ?{idx}"));
            args.push(Box::new(num));
        }
    }

    let mut join_fts = String::new();
    if fts != "\"\"" {
        let idx = args.len() + 1;
        join_fts = format!(
            "JOIN (
                SELECT conversation_id, MIN(rank) AS r FROM (
                    SELECT conversation_id, rank FROM fts_conversations WHERE fts_conversations MATCH ?{idx}
                    UNION ALL
                    SELECT f.conversation_id, rank FROM fts_threads f WHERE fts_threads MATCH ?{idx}
                    UNION ALL
                    SELECT c2.id, rank FROM fts_ai_analyses a JOIN conversations c2 ON c2.id = a.conversation_id WHERE fts_ai_analyses MATCH ?{idx}
                ) GROUP BY conversation_id
            ) m ON m.conversation_id = c.id"
        );
        args.push(Box::new(fts));
    }
    let rank_expr = if join_fts.is_empty() {
        "0"
    } else {
        "COALESCE(m.r, 999999)"
    };
    let limit_idx = args.len() + 1;
    args.push(Box::new(limit));

    let sql = format!(
        "SELECT c.id, c.number, c.subject, c.preview, c.status, c.created_at,
           TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, '')) AS customer,
           COALESCE((SELECT ce.value FROM customer_emails ce WHERE ce.customer_id = cu.id LIMIT 1), '') AS customer_email,
           {rank_expr} AS rank
         FROM conversations c
         LEFT JOIN customers cu ON cu.id = c.customer_local_id
         {join_fts}
         WHERE {}
         ORDER BY rank ASC, c.created_at DESC
         LIMIT ?{limit_idx}",
        where_sql.join(" AND ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let arg_refs: Vec<&dyn ToSql> = args.iter().map(Box::as_ref).collect();
    let rows = stmt
        .query_map(arg_refs.as_slice(), |r| {
            let id: i64 = r.get(0)?;
            let number: i64 = r.get(1)?;
            let subject: Option<String> = r.get(2)?;
            Ok(SearchHit {
                scope: "tickets".into(),
                id,
                title: format!(
                    "#{number} {}",
                    subject.unwrap_or_else(|| "(no subject)".into())
                ),
                subtitle: [
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(4)?,
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
                snippet: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                score: 1.0,
                href: format!("/inbox/conversation/{id}"),
                why: vec!["keyword match".into()],
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Recent-conversations fallback — reference `recentConversations`.
fn recent_conversations(conn: &Connection, limit: i64) -> Result<Vec<SearchHit>> {
    let mut stmt = conn.prepare(
        "SELECT id, number, subject, preview, status
           FROM conversations
          WHERE deleted_at IS NULL AND merged_into_conversation_id IS NULL
          ORDER BY updated_at DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| {
            let id: i64 = r.get(0)?;
            let number: i64 = r.get(1)?;
            Ok(SearchHit {
                scope: "tickets".into(),
                id,
                title: format!(
                    "#{number} {}",
                    r.get::<_, Option<String>>(2)?
                        .unwrap_or_else(|| "(no subject)".into())
                ),
                subtitle: r.get(4)?,
                snippet: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                score: 0.5,
                href: format!("/inbox/conversation/{id}"),
                why: vec!["recent".into()],
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn status_applies(f: &SearchFilters) -> bool {
    matches!(f.status.as_deref(), Some(s) if !s.is_empty() && s != "all")
}

/// Reference `hasFilters` — JS truthiness semantics included.
fn has_filters(f: &SearchFilters) -> bool {
    status_applies(f)
        || f.mailbox_id.is_some_and(|v| v != 0)
        || f.tag.as_deref().is_some_and(|t| !t.is_empty())
        || f.since_days.is_some_and(|v| v != 0)
        || f.assignee_id.is_some_and(|v| v != 0)
}

// ─── Customers ────────────────────────────────────────────────────────────

/// Search customers — reference `searchCustomers`.
///
/// LIKE-based (with `\%_` escaped) over first/last name and
/// `customer_emails.value`, ordered by last name.
pub fn search_customers(conn: &Connection, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let like = format!("%{}%", escape_like(query));
    let mut stmt = conn.prepare(
        "SELECT c.id, c.first_name, c.last_name,
           (SELECT ce.value FROM customer_emails ce WHERE ce.customer_id = c.id LIMIT 1) AS email,
           o.name AS org,
           (SELECT COUNT(*) FROM conversations cv WHERE cv.customer_local_id = c.id) AS conv_count
         FROM customers c LEFT JOIN organizations o ON o.id = c.organization_id
         WHERE c.deleted_at IS NULL AND (
           c.first_name LIKE ?1 ESCAPE '\\' OR c.last_name LIKE ?1 ESCAPE '\\' OR
           EXISTS (SELECT 1 FROM customer_emails ce WHERE ce.customer_id = c.id AND ce.value LIKE ?1 ESCAPE '\\')
         )
         ORDER BY c.last_name LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![like, limit], |r| {
            let id: i64 = r.get(0)?;
            let first: Option<String> = r.get(1)?;
            let last: Option<String> = r.get(2)?;
            let email: Option<String> = r.get(3)?;
            let org: Option<String> = r.get(4)?;
            let conv_count: i64 = r.get(5)?;
            let title = [first, last]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            Ok(SearchHit {
                scope: "customers".into(),
                id,
                title: if title.is_empty() {
                    "Unknown".into()
                } else {
                    title
                },
                subtitle: email.unwrap_or_default(),
                snippet: match org.filter(|o| !o.is_empty()) {
                    Some(org) => format!("{org} · {conv_count} conversations"),
                    None => format!("{conv_count} conversations"),
                },
                score: 1.0,
                href: format!("/customers/{id}"),
                why: vec!["name/email match".into()],
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ─── Knowledge ────────────────────────────────────────────────────────────

/// Raw knowledge hits with document id + visibility — reference
/// `searchKnowledgeRaw` (used by the evidence builder + knowledge API).
///
/// `mode` is `"and"` (default) or `"or"`; the snippet is FTS5
/// `snippet(fts_knowledge, 1, '[', ']', '…', 14)` over the content column.
#[allow(clippy::ref_option)]
pub fn search_knowledge_raw(
    conn: &Connection,
    query: &str,
    visibility: Option<&str>,
    limit: i64,
    mode: &str,
) -> Result<Vec<KnowledgeRawHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let fts = if mode == "or" {
        fts_query_or(query, 10)
    } else {
        fts_query(query)
    };
    let vis_sql = visibility.map_or("", |_| "AND f.visibility = ?2");
    let limit_idx = if visibility.is_some() { 3 } else { 2 };
    let sql = format!(
        "SELECT f.document_id, f.title, snippet(fts_knowledge, 1, '[', ']', '…', 14) AS snippet, f.visibility
           FROM fts_knowledge f WHERE fts_knowledge MATCH ?1 {vis_sql} ORDER BY rank LIMIT ?{limit_idx}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<KnowledgeRawHit> {
        Ok(KnowledgeRawHit {
            document_id: r.get(0)?,
            title: r.get(1)?,
            snippet: r.get(2)?,
            visibility: r.get(3)?,
        })
    };
    let rows = match visibility {
        Some(v) => stmt.query_map(params![fts, v, limit], map_row)?,
        None => stmt.query_map(params![fts, limit], map_row)?,
    }
    .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Knowledge hits — reference `searchKnowledge`.
pub fn search_knowledge(
    conn: &Connection,
    query: &str,
    visibility: Option<&str>,
    limit: i64,
) -> Result<Vec<SearchHit>> {
    Ok(search_knowledge_raw(conn, query, visibility, limit, "and")?
        .into_iter()
        .map(|r| SearchHit {
            scope: "knowledge".into(),
            id: r.document_id,
            title: r.title,
            subtitle: format!(
                "Knowledge · {}",
                if r.visibility == "customer_safe" {
                    "customer-safe"
                } else {
                    "internal"
                }
            ),
            snippet: r.snippet,
            score: 1.0,
            href: format!("/knowledge?doc={}", r.document_id),
            why: vec!["knowledge match".into()],
        })
        .collect())
}

// ─── Known issues / saved replies / AI analyses ──────────────────────────

/// Known-issue hits — reference `searchKnownIssues`
/// (`snippet(fts_known_issues, 0, …)` over the title column).
pub fn search_known_issues(conn: &Connection, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    if !table_exists(conn, "known_issues")? {
        return Ok(Vec::new());
    }
    let fts = fts_query(query);
    let mut stmt = conn.prepare(
        "SELECT ki.id, ki.title, snippet(fts_known_issues, 0, '[', ']', '…', 14) AS snippet
           FROM fts_known_issues f JOIN known_issues ki ON ki.id = f.known_issue_id
          WHERE fts_known_issues MATCH ?1 ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![fts, limit], |r| {
            Ok(SearchHit {
                scope: "issues".into(),
                id: r.get(0)?,
                title: r.get(1)?,
                subtitle: "Known issue".into(),
                snippet: r.get(2)?,
                score: 1.0,
                href: "/issues?tab=known".into(),
                why: vec!["known issue match".into()],
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Saved-reply hits — reference `searchSavedReplies`
/// (`snippet(fts_saved_replies, 1, …)` over the preview column; soft-deleted
/// replies excluded).
pub fn search_saved_replies(conn: &Connection, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    if !table_exists(conn, "saved_replies")? {
        return Ok(Vec::new());
    }
    let fts = fts_query(query);
    let mut stmt = conn.prepare(
        "SELECT sr.id, sr.name, snippet(fts_saved_replies, 1, '[', ']', '…', 14) AS snippet
           FROM fts_saved_replies f JOIN saved_replies sr ON sr.id = f.saved_reply_id
          WHERE fts_saved_replies MATCH ?1 AND sr.deleted_at IS NULL ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![fts, limit], |r| {
            Ok(SearchHit {
                scope: "saved_replies".into(),
                id: r.get(0)?,
                title: r.get(1)?,
                subtitle: "Saved reply".into(),
                snippet: r.get(2)?,
                score: 1.0,
                href: "/inbox".into(),
                why: vec!["saved reply match".into()],
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// AI-analysis hits — reference `searchAiAnalyses`
/// (`snippet(fts_ai_analyses, 0, …)` over the summary column; analyses on
/// soft-deleted conversations excluded).
pub fn search_ai_analyses(conn: &Connection, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let fts = fts_query(query);
    let mut stmt = conn.prepare(
        "SELECT a.conversation_id, c.number, snippet(fts_ai_analyses, 0, '[', ']', '…', 14) AS snippet, c.subject
           FROM fts_ai_analyses a JOIN conversations c ON c.id = a.conversation_id
          WHERE fts_ai_analyses MATCH ?1 AND c.deleted_at IS NULL ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![fts, limit], |r| {
            let conversation_id: i64 = r.get(0)?;
            let number: i64 = r.get(1)?;
            let subject: Option<String> = r.get(3)?;
            Ok(SearchHit {
                scope: "ai".into(),
                id: conversation_id,
                title: format!("AI analysis · #{number} {}", subject.unwrap_or_default()),
                subtitle: "AI-derived (marked as AI-generated)".into(),
                snippet: r.get(2)?,
                score: 1.0,
                href: format!("/inbox/conversation/{conversation_id}"),
                why: vec!["AI analysis match".into()],
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ─── Unified search ───────────────────────────────────────────────────────

/// Unified search across scopes — reference `SearchEngine.search`.
///
/// `scope` is one of [`SEARCH_SCOPES`] (an empty scope is treated as `all`,
/// matching the reference default). Unknown scopes produce an empty
/// response, like the reference.
pub fn search(
    conn: &Connection,
    query: &str,
    scope: &str,
    filters: &SearchFilters,
) -> Result<SearchResponse> {
    apply_fts_migration(conn)?;
    let scope = if scope.is_empty() { "all" } else { scope };
    let mut hits = Vec::new();
    if scope == "all" || scope == "tickets" {
        hits.extend(search_conversations(conn, query, filters, 40)?);
    }
    if scope == "all" || scope == "customers" {
        hits.extend(search_customers(conn, query, 15)?);
    }
    if scope == "all" || scope == "knowledge" {
        hits.extend(search_knowledge(conn, query, None, 10)?);
    }
    if scope == "all" || scope == "issues" {
        hits.extend(search_known_issues(conn, query, 10)?);
    }
    if scope == "all" || scope == "saved_replies" {
        hits.extend(search_saved_replies(conn, query, 10)?);
    }
    if scope == "all" || scope == "ai" {
        hits.extend(search_ai_analyses(conn, query, 10)?);
    }
    let total = hits.len();
    Ok(SearchResponse {
        query: query.to_string(),
        hits,
        total,
        used_semantic: false,
        semantic_available: false,
        mode_note: None,
    })
}

/// Legacy universal search — thin wrapper over [`search`] that keeps the old
/// `SearchResult` shape for the IPC command, the knowledge route and the
/// hybrid layer. Preserves the legacy hardening: 500-char query cap and
/// empty-query → empty result.
pub fn universal_search(conn: &Connection, query: &str) -> Result<Vec<SearchResult>> {
    let query = cap_query(query);
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let response = search(conn, query, "all", &SearchFilters::default())?;
    Ok(response
        .hits
        .into_iter()
        .map(|h| SearchResult {
            resource_type: h.scope,
            remote_id: h.id,
            title: h.title,
            snippet: h.snippet,
        })
        .collect())
}

/// Truncate to `MAX_SEARCH_QUERY_LENGTH` bytes on a char boundary.
fn cap_query(query: &str) -> &str {
    if query.len() <= MAX_SEARCH_QUERY_LENGTH {
        return query;
    }
    let mut end = MAX_SEARCH_QUERY_LENGTH;
    while !query.is_char_boundary(end) {
        end -= 1;
    }
    &query[..end]
}

// ─── Indexing callers (delete + insert) ───────────────────────────────────

/// Index one conversation into `fts_conversations` (subject, preview,
/// customer name, tags csv, number) and re-index its threads into
/// `fts_threads` — reference `conversationRepo.reindexConversationFts`.
///
/// A no-op (after deleting any stale row) when the conversation no longer
/// exists.
pub fn index_conversation_fts(conn: &Connection, local_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_conversations WHERE conversation_id = ?1",
        params![local_id],
    )?;
    let row = conn
        .query_row(
            "SELECT c.id, c.number, c.subject, c.preview,
               TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, '')) AS customer,
               (SELECT GROUP_CONCAT(t.name) FROM conversation_tags ct
                  JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id) AS tags
             FROM conversations c LEFT JOIN customers cu ON cu.id = c.customer_local_id
            WHERE c.id = ?1",
            params![local_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?;
    if let Some((id, number, subject, preview, customer, tags)) = row {
        conn.execute(
            "INSERT INTO fts_conversations (subject, preview, customer, tags, numbers, conversation_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                subject.unwrap_or_default(),
                preview.unwrap_or_default(),
                customer.unwrap_or_default(),
                tags.unwrap_or_default(),
                number.to_string(),
                id
            ],
        )?;
        // Re-index the conversation's threads (published, non-empty bodies).
        conn.execute(
            "DELETE FROM fts_threads WHERE conversation_id = ?1",
            params![local_id],
        )?;
        if table_exists(conn, "conversation_threads")? {
            conn.execute(
                "INSERT INTO fts_threads (body, thread_id, conversation_id)
                 SELECT t.body_text, t.id, t.conversation_id FROM conversation_threads t
                  WHERE t.conversation_id = ?1 AND t.body_text IS NOT NULL AND LENGTH(t.body_text) > 0
                    AND t.deleted_at IS NULL",
                params![local_id],
            )?;
        }
    }
    Ok(())
}

/// Index one thread into `fts_threads` — reference
/// `conversationRepo.upsertThread`'s FTS branch (v1.6.0 audit fix: the stale
/// row is ALWAYS deleted, a new one only inserted when the trimmed body is
/// non-empty, and `fts_indexed` is reset to 0 otherwise so blanked bodies
/// cannot keep ghost hits).
pub fn index_thread_fts(conn: &Connection, thread_local_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_threads WHERE thread_id = ?1",
        params![thread_local_id],
    )?;
    let row = conn
        .query_row(
            "SELECT body_text, conversation_id FROM conversation_threads WHERE id = ?1",
            params![thread_local_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    // 1 when the body was indexed, 0 when the thread exists but has no
    // indexable body; no update at all when the thread is gone.
    let mut flag: Option<i64> = None;
    if let Some((Some(body), conversation_id)) = row {
        if !body.trim().is_empty() {
            conn.execute(
                "INSERT INTO fts_threads (body, thread_id, conversation_id) VALUES (?1, ?2, ?3)",
                params![body, thread_local_id, conversation_id],
            )?;
            flag = Some(1);
        } else {
            flag = Some(0);
        }
    }
    if let Some(flag) = flag {
        if column_exists(conn, "conversation_threads", "fts_indexed")? {
            conn.execute(
                "UPDATE conversation_threads SET fts_indexed = ?1 WHERE id = ?2",
                params![flag, thread_local_id],
            )?;
        }
    }
    Ok(())
}

/// Index one knowledge chunk into `fts_knowledge` (title from the owning
/// document, visibility carried alongside) — reference `knowledgeRepo`
/// delete+insert pattern, keyed per chunk.
pub fn index_knowledge_chunk_fts(conn: &Connection, chunk_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_knowledge WHERE chunk_id = ?1",
        params![chunk_id],
    )?;
    let row = conn
        .query_row(
            "SELECT c.content, c.document_id, d.title, d.visibility
               FROM knowledge_chunks c JOIN knowledge_documents d ON d.id = c.document_id
              WHERE c.id = ?1",
            params![chunk_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((content, document_id, title, visibility)) = row {
        conn.execute(
            "INSERT INTO fts_knowledge (title, content, chunk_id, document_id, visibility)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![title, content, chunk_id, document_id, visibility],
        )?;
    }
    conn.execute(
        "UPDATE knowledge_chunks SET fts_indexed = 1 WHERE id = ?1",
        params![chunk_id],
    )?;
    Ok(())
}

/// Index one known issue into `fts_known_issues` — reference `issueRepo`
/// delete+insert pattern.
pub fn index_known_issue_fts(conn: &Connection, known_issue_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_known_issues WHERE known_issue_id = ?1",
        params![known_issue_id],
    )?;
    let row = conn
        .query_row(
            "SELECT COALESCE(title, ''), COALESCE(symptoms, ''), COALESCE(workaround, ''),
                    COALESCE(customer_safe_explanation, '')
               FROM known_issues WHERE id = ?1",
            params![known_issue_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((title, symptoms, workaround, explanation)) = row {
        conn.execute(
            "INSERT INTO fts_known_issues (title, symptoms, workaround, customer_safe_explanation, known_issue_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![title, symptoms, workaround, explanation, known_issue_id],
        )?;
    }
    Ok(())
}

/// Index one saved reply into `fts_saved_replies` — reference
/// `referenceRepo` delete+insert pattern.
pub fn index_saved_reply_fts(conn: &Connection, saved_reply_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_saved_replies WHERE saved_reply_id = ?1",
        params![saved_reply_id],
    )?;
    let row = conn
        .query_row(
            "SELECT name, COALESCE(preview, ''), COALESCE(text, '') FROM saved_replies WHERE id = ?1",
            params![saved_reply_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((name, preview, text)) = row {
        conn.execute(
            "INSERT INTO fts_saved_replies (saved_reply_id, name, preview, text) VALUES (?1, ?2, ?3, ?4)",
            params![saved_reply_id, name, preview, text],
        )?;
    }
    Ok(())
}

/// Index one AI analysis into `fts_ai_analyses` — the port's equivalent of
/// reference `aiRepo.saveAnalysis`'s FTS insert (analyses live in
/// `ai_extracted_facts` keyed rows; callers that save an analysis call this
/// with the summary / primary_question / intent values).
pub fn index_ai_analysis_fts(
    conn: &Connection,
    conversation_id: i64,
    run_id: i64,
    summary: Option<&str>,
    primary_question: Option<&str>,
    intent: Option<&str>,
) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_ai_analyses WHERE conversation_id = ?1 AND run_id = ?2",
        params![conversation_id, run_id],
    )?;
    conn.execute(
        "INSERT INTO fts_ai_analyses (summary, primary_question, intent, conversation_id, run_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            summary.unwrap_or(""),
            primary_question.unwrap_or(""),
            intent.unwrap_or(""),
            conversation_id,
            run_id
        ],
    )?;
    Ok(())
}

// ─── Rebuild (maintenance job) ────────────────────────────────────────────

/// Atomic rebuild of every FTS table from its base tables — the reference
/// `rebuild_search_index` maintenance job, expanded to all 8 reference
/// tables plus the legacy pair.
///
/// - `fts_conversations`: one delete+insert per non-deleted conversation
///   (which also re-indexes its threads), like the reference job.
/// - `fts_threads`: bulk insert of published non-empty bodies, then
///   `fts_indexed = 1` for every thread with a body.
/// - `fts_knowledge`: chunks JOIN documents (title/visibility from the doc).
/// - `fts_known_issues`, `fts_saved_replies` (deleted excluded): full copies.
/// - `fts_ai_analyses`: backfilled from `ai_extracted_facts` per
///   (conversation, run) — the port stores analyses as keyed fact rows.
/// - `docs_fts`: live docs; `fts_custom_objects`: title + search_text
///   rebuilt from `data_json` values.
/// - Records `fts_version = 3` in `application_settings`.
///
/// Returns the number of conversations re-indexed. Tables whose base table
/// does not exist (yet) are wiped and left empty — search stays consistent.
pub fn rebuild_indexes(conn: &Connection) -> Result<usize> {
    apply_fts_migration(conn)?;
    conn.execute_batch("BEGIN")?;
    let result = rebuild_indexes_tx(conn);
    match result {
        Ok(n) => {
            conn.execute_batch("COMMIT")?;
            Ok(n)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn rebuild_indexes_tx(conn: &Connection) -> Result<usize> {
    for table in [
        "fts_conversations",
        "fts_threads",
        "fts_knowledge",
        "fts_known_issues",
        "fts_saved_replies",
        "fts_ai_analyses",
        "docs_fts",
        "fts_custom_objects",
        "conversations_fts",
        "customers_fts",
    ] {
        conn.execute(&format!("DELETE FROM {table}"), [])?;
    }

    // fts_conversations (+ per-conversation thread reindex).
    let conv_ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM conversations WHERE deleted_at IS NULL")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for id in &conv_ids {
        index_conversation_fts(conn, *id)?;
    }
    let conv_count = conv_ids.len();

    // fts_threads (reference job semantics: published, non-empty bodies).
    if table_exists(conn, "conversation_threads")? {
        conn.execute(
            "INSERT INTO fts_threads (body, thread_id, conversation_id)
             SELECT t.body_text, t.id, t.conversation_id FROM conversation_threads t
              WHERE t.body_text IS NOT NULL AND LENGTH(t.body_text) > 0 AND t.deleted_at IS NULL",
            [],
        )?;
        conn.execute(
            "UPDATE conversation_threads SET fts_indexed = 1
              WHERE body_text IS NOT NULL AND LENGTH(body_text) > 0",
            [],
        )?;
    }

    // fts_knowledge (title + visibility from the owning document).
    conn.execute(
        "INSERT INTO fts_knowledge (title, content, chunk_id, document_id, visibility)
         SELECT d.title, c.content, c.id, c.document_id, d.visibility
           FROM knowledge_chunks c JOIN knowledge_documents d ON d.id = c.document_id",
        [],
    )?;
    conn.execute("UPDATE knowledge_chunks SET fts_indexed = 1", [])?;

    // fts_known_issues (the port has no deleted_at on known_issues).
    if table_exists(conn, "known_issues")? {
        conn.execute(
            "INSERT INTO fts_known_issues (title, symptoms, workaround, customer_safe_explanation, known_issue_id)
             SELECT COALESCE(title, ''), COALESCE(symptoms, ''), COALESCE(workaround, ''),
                    COALESCE(customer_safe_explanation, ''), id
               FROM known_issues",
            [],
        )?;
    }

    // fts_saved_replies (soft-deleted excluded, like the search JOIN).
    if table_exists(conn, "saved_replies")? {
        conn.execute(
            "INSERT INTO fts_saved_replies (saved_reply_id, name, preview, text)
             SELECT id, name, COALESCE(preview, ''), COALESCE(text, '')
               FROM saved_replies WHERE deleted_at IS NULL",
            [],
        )?;
    }

    // fts_ai_analyses backfill from extracted facts (one row per
    // conversation + run that has at least one of the three indexed keys).
    conn.execute(
        "INSERT INTO fts_ai_analyses (summary, primary_question, intent, conversation_id, run_id)
         SELECT COALESCE(MAX(CASE WHEN key = 'summary' THEN value END), ''),
                COALESCE(MAX(CASE WHEN key = 'primary_question' THEN value END), ''),
                COALESCE(MAX(CASE WHEN key = 'intent' THEN value END), ''),
                conversation_id, run_id
           FROM ai_extracted_facts
          WHERE key IN ('summary', 'primary_question', 'intent') AND conversation_id IS NOT NULL
          GROUP BY conversation_id, run_id",
        [],
    )?;

    // docs_fts (soft-deleted articles excluded).
    if table_exists(conn, "docs_articles")? {
        conn.execute(
            "INSERT INTO docs_fts (name, text, article_id)
             SELECT name, COALESCE(text, ''), id FROM docs_articles WHERE deleted_at IS NULL",
            [],
        )?;
    }

    // fts_custom_objects (search_text rebuilt from title + JSON values).
    if table_exists(conn, "custom_objects")? {
        rebuild_custom_objects_fts(conn)?;
    }

    // Legacy pre-parity tables.
    conn.execute(
        "INSERT INTO conversations_fts (remote_id, subject, preview)
         SELECT remote_id, COALESCE(subject, ''), COALESCE(preview, '')
           FROM conversations WHERE deleted_at IS NULL",
        [],
    )?;
    conn.execute(
        "INSERT INTO customers_fts (remote_id, first_name, last_name, email, organization)
         SELECT remote_id, COALESCE(first_name, ''), COALESCE(last_name, ''), COALESCE(email, ''), COALESCE(organization, '')
           FROM customers WHERE deleted_at IS NULL",
        [],
    )?;

    // Reference rebuild job records the index version.
    if table_exists(conn, "application_settings")? {
        conn.execute(
            "INSERT OR REPLACE INTO application_settings (key, value) VALUES ('fts_version', '3')",
            [],
        )?;
    }
    Ok(conv_count)
}

/// Rebuild `fts_custom_objects` — search_text from title + JSON property
/// values (the port stores `data_json`; the reference stores a precomputed
/// `search_text` column).
fn rebuild_custom_objects_fts(conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT o.id, o.title, o.data_json
               FROM custom_objects o
               JOIN custom_object_types t ON t.id = o.type_id
              WHERE o.deleted_at IS NULL",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, title, data_json) in rows {
        let search_text = build_custom_object_search_text(&title, data_json.as_deref());
        conn.execute(
            "INSERT INTO fts_custom_objects (title, search_text, object_id) VALUES (?1, ?2, ?3)",
            params![title, search_text, id],
        )?;
    }
    Ok(())
}

/// Reference `buildSearchText`: title + property values, capped at 5000 chars.
fn build_custom_object_search_text(title: &str, data_json: Option<&str>) -> String {
    let mut parts = vec![title.to_string()];
    if let Some(json) = data_json {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(json) {
            for value in map.values() {
                match value {
                    serde_json::Value::String(s) => parts.push(s.clone()),
                    serde_json::Value::Number(n) => parts.push(n.to_string()),
                    serde_json::Value::Bool(b) => parts.push(b.to_string()),
                    _ => {}
                }
            }
        }
    }
    let joined = parts.join(" ");
    if joined.chars().count() > 5000 {
        joined.chars().take(5000).collect()
    } else {
        joined
    }
}

// ─── Legacy indexing (pre-parity tables) ──────────────────────────────────

/// Legacy: index a conversation into `conversations_fts` (old callers key by
/// remote_id, e.g. the perf dataset). Also keeps the reference-shaped
/// `fts_conversations` in step when the row exists locally.
pub fn index_conversation(
    conn: &Connection,
    remote_id: i64,
    subject: &str,
    preview: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM conversations_fts WHERE remote_id = ?1",
        params![remote_id],
    )?;
    conn.execute(
        "INSERT INTO conversations_fts (remote_id, subject, preview) VALUES (?1, ?2, ?3)",
        params![remote_id, subject, preview],
    )?;
    if let Ok(local_id) = conn.query_row(
        "SELECT id FROM conversations WHERE remote_id = ?1",
        params![remote_id],
        |r| r.get::<_, i64>(0),
    ) {
        index_conversation_fts(conn, local_id)?;
    }
    Ok(())
}

/// Legacy: index a customer into `customers_fts` (old callers key by
/// remote_id). The reference engine searches customers via LIKE, so no
/// reference-shaped customer FTS table exists.
pub fn index_customer(
    conn: &Connection,
    remote_id: i64,
    first_name: &str,
    last_name: &str,
    email: &str,
    organization: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM customers_fts WHERE remote_id = ?1",
        params![remote_id],
    )?;
    conn.execute(
        "INSERT INTO customers_fts (remote_id, first_name, last_name, email, organization)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![remote_id, first_name, last_name, email, organization],
    )?;
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// Full stack (like the boot path, minus M039 — apply_fts_migration
    /// ensures the M039 shapes itself, which is exactly what we test).
    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        // The canonical boot chain (bootstrap.rs contract: every path uses
        // apply_all; this fixture predates it and hand-copied the steps,
        // which stopped at M029's `docs` name before DB-02's m044).
        crate::bootstrap::apply_all(&mut conn).unwrap();
        // DB-03 (M047): conversations carries real FKs
        // (mailbox_local_id→mailboxes, customer_local_id→customers,
        // assignee_local_id→users); the Conv fixture below defaults to
        // mailbox 1 / customer 2001 and the filter test uses mailbox 2 /
        // assignee 7, so the parents must exist.
        conn.execute_batch(
            "INSERT OR IGNORE INTO mailboxes (id, remote_id, name) VALUES
               (1, 1, 'Support'), (2, 2, 'Billing'), (101, 101, 'Legacy');
             INSERT OR IGNORE INTO customers (id, remote_id, first_name, last_name)
               VALUES (2001, 2001, 'Default', 'Customer');
             INSERT OR IGNORE INTO users (id, remote_id, first_name, last_name)
               VALUES (7, 7, 'Agent', 'Seven');",
        )
        .unwrap();
        conn
    }

    /// Minimal DB (like the hybrid_search/perf_guards test harnesses):
    /// base migrations + the FTS layer only.
    fn minimal_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        apply_fts_migration(&conn).unwrap();
        // DB-03 (M047): the reference column names on conversations. The
        // slim chain can't run m047's converging copy (it expects the full
        // pre-M047 shape), so the fixture reproduces the renamed result
        // directly — plain renames, no FK clauses (the slim chain never
        // declared them; SQLite rewrites the m001 index defs itself).
        let _ = conn.execute(
            "ALTER TABLE conversations RENAME COLUMN mailbox_id TO mailbox_local_id",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE conversations RENAME COLUMN assignee_id TO assignee_local_id",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE conversations RENAME COLUMN customer_id TO customer_local_id",
            [],
        );
        conn
    }

    struct Conv<'a> {
        subject: &'a str,
        preview: &'a str,
        status: &'a str,
        mailbox_id: i64,
        assignee_id: Option<i64>,
        // FK'd to customers(id) after M047 — fresh_db() seeds the default
        // customer 2001 (and add_customer-created ids exist by construction);
        // the reference's LEFT JOIN then just yields an empty customer for
        // conversations without one.
        customer_id: i64,
        created_at: &'a str,
        updated_at: &'a str,
        number: i64,
    }

    impl Default for Conv<'_> {
        fn default() -> Self {
            Self {
                subject: "Support request",
                preview: "Customer reports an issue",
                status: "active",
                mailbox_id: 1,
                assignee_id: None,
                customer_id: 2001,
                created_at: "2025-01-01T10:00:00Z",
                updated_at: "2025-01-02T10:00:00Z",
                number: 0,
            }
        }
    }

    fn add_conversation(conn: &Connection, seed: Conv<'_>) -> i64 {
        let remote_id: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(remote_id), 0) + 1 FROM conversations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let number = if seed.number == 0 {
            remote_id
        } else {
            seed.number
        };
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, preview, status, mailbox_local_id,
                                        assignee_local_id, customer_local_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                remote_id,
                number,
                seed.subject,
                seed.preview,
                seed.status,
                seed.mailbox_id,
                seed.assignee_id,
                seed.customer_id,
                seed.created_at,
                seed.updated_at
            ],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        index_conversation_fts(conn, id).unwrap();
        id
    }

    fn add_customer(
        conn: &Connection,
        first: &str,
        last: &str,
        email: &str,
        org: Option<&str>,
    ) -> i64 {
        let remote_id: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(remote_id), 0) + 1 FROM customers",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO customers (remote_id, first_name, last_name, email, organization)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![remote_id, first, last, email, org],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        if !email.is_empty() {
            conn.execute(
                "INSERT INTO customer_emails (customer_id, value, type) VALUES (?1, ?2, 'work')",
                params![id, email],
            )
            .unwrap();
        }
        if let Some(org) = org.filter(|o| !o.is_empty()) {
            conn.execute(
                "INSERT INTO organizations (remote_id, name) VALUES (?1, ?2)",
                params![remote_id + 10_000, org],
            )
            .unwrap();
            let org_id = conn.last_insert_rowid();
            conn.execute(
                "UPDATE customers SET organization_id = ?1 WHERE id = ?2",
                params![org_id, id],
            )
            .unwrap();
        }
        id
    }

    fn add_thread(conn: &Connection, conversation_id: i64, body: &str) -> i64 {
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type)
             VALUES (?1, 'customer', ?2, 'customer')",
            params![conversation_id, body],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        index_thread_fts(conn, id).unwrap();
        id
    }

    fn add_knowledge_doc(conn: &Connection, title: &str, content: &str, visibility: &str) -> i64 {
        conn.execute(
            "INSERT INTO knowledge_sources (name, kind, visibility) VALUES ('test', 'import', ?1)",
            params![visibility],
        )
        .unwrap();
        let source = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO knowledge_documents (source_id, title, visibility, content)
             VALUES (?1, ?2, ?3, ?4)",
            params![source, title, visibility, content],
        )
        .unwrap();
        let doc = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO knowledge_chunks (document_id, chunk_index, content) VALUES (?1, 0, ?2)",
            params![doc, content],
        )
        .unwrap();
        let chunk = conn.last_insert_rowid();
        index_knowledge_chunk_fts(conn, chunk).unwrap();
        doc
    }

    fn add_known_issue(conn: &Connection, title: &str, symptoms: &str) -> i64 {
        conn.execute(
            "INSERT INTO known_issues (name, title, symptoms, workaround, customer_safe_explanation, status)
             VALUES (?1, ?1, ?2, 'w', 'c', 'active')",
            params![title, symptoms],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        index_known_issue_fts(conn, id).unwrap();
        id
    }

    fn add_saved_reply(conn: &Connection, name: &str, preview: &str, text: &str) -> i64 {
        conn.execute(
            "INSERT INTO saved_replies (name, preview, text) VALUES (?1, ?2, ?3)",
            params![name, preview, text],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        index_saved_reply_fts(conn, id).unwrap();
        id
    }

    fn add_ai_run(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json)
             VALUES ('h', 'v', 'm', '{}')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    // ---- fts_query tokenization (reference ftsQuery/ftsQueryOr) ----------

    #[test]
    fn fts_query_empty_and_specials_only() {
        assert_eq!(fts_query(""), "\"\"");
        assert_eq!(fts_query("   "), "\"\"");
        assert_eq!(fts_query("\"*()"), "\"\"");
        assert_eq!(fts_query_or("", 10), "\"\"");
    }

    #[test]
    fn fts_query_strips_specials_and_quotes_prefix() {
        assert_eq!(fts_query("hello (world)*"), "\"hello\"* \"world\"*");
        assert_eq!(fts_query("say \"hi\""), "\"say\"* \"hi\"*");
    }

    #[test]
    fn fts_query_caps_at_8_tokens() {
        let q = "one two three four five six seven eight nine ten";
        assert_eq!(
            fts_query(q),
            "\"one\"* \"two\"* \"three\"* \"four\"* \"five\"* \"six\"* \"seven\"* \"eight\"*"
        );
    }

    #[test]
    fn fts_query_or_requires_more_than_two_chars() {
        assert_eq!(fts_query_or("a bb ccc dd", 10), "\"ccc\"*");
    }

    #[test]
    fn fts_query_or_joins_with_or() {
        assert_eq!(
            fts_query_or("aaa bbb ccc", 10),
            "\"aaa\"* OR \"bbb\"* OR \"ccc\"*"
        );
    }

    #[test]
    fn fts_query_or_caps_at_max_tokens() {
        let q = "aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll";
        assert_eq!(
            fts_query_or(q, 10),
            "\"aaa\"* OR \"bbb\"* OR \"ccc\"* OR \"ddd\"* OR \"eee\"* OR \"fff\"* OR \"ggg\"* OR \"hhh\"* OR \"iii\"* OR \"jjj\"*"
        );
    }

    #[test]
    fn fts_query_injection_attempt_is_quoted_safely() {
        // Quotes/parens are stripped; the rest becomes quoted prefix phrases.
        assert_eq!(fts_query("test\" OR 1=1--"), "\"test\"* \"OR\"* \"1=1--\"*");
    }

    // ---- conversations ----------------------------------------------------

    #[test]
    fn search_finds_conversation_by_subject() {
        let conn = fresh_db();
        let id = add_conversation(
            &conn,
            Conv {
                subject: "Billing issue with invoice",
                preview: "Customer reports wrong amount",
                ..Default::default()
            },
        );
        let hits = search_conversations(&conn, "billing", &SearchFilters::default(), 40).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        assert_eq!(hits[0].scope, "tickets");
        assert_eq!(hits[0].title, "#1 Billing issue with invoice");
        assert_eq!(hits[0].snippet, "Customer reports wrong amount");
        assert_eq!(hits[0].href, format!("/inbox/conversation/{id}"));
        assert_eq!(hits[0].why, vec!["keyword match".to_string()]);
    }

    #[test]
    fn search_finds_conversation_by_thread_body() {
        let conn = fresh_db();
        let id = add_conversation(
            &conn,
            Conv {
                subject: "Unrelated subject",
                ..Default::default()
            },
        );
        add_thread(&conn, id, "The refund was processed yesterday");
        let hits = search_conversations(&conn, "refund", &SearchFilters::default(), 40).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
    }

    #[test]
    fn blanked_thread_body_drops_ghost_hit_and_resets_flag() {
        // Reference v1.6.0 audit fix: a thread whose body becomes blank must
        // not keep its old indexed text (ghost hits), and fts_indexed resets.
        let conn = fresh_db();
        let id = add_conversation(
            &conn,
            Conv {
                subject: "Unrelated subject",
                ..Default::default()
            },
        );
        let thread = add_thread(&conn, id, "The refund was processed yesterday");
        assert!(
            !search_conversations(&conn, "refund", &SearchFilters::default(), 40)
                .unwrap()
                .is_empty()
        );
        // Whitespace-only body: not indexable.
        conn.execute(
            "UPDATE conversation_threads SET body_text = '   ' WHERE id = ?1",
            params![thread],
        )
        .unwrap();
        index_thread_fts(&conn, thread).unwrap();
        assert!(
            search_conversations(&conn, "refund", &SearchFilters::default(), 40)
                .unwrap()
                .is_empty()
        );
        let flag: i64 = conn
            .query_row(
                "SELECT fts_indexed FROM conversation_threads WHERE id = ?1",
                params![thread],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flag, 0);
    }

    #[test]
    fn search_conversation_pure_number_matches_number_column() {
        let conn = fresh_db();
        add_conversation(
            &conn,
            Conv {
                subject: "Something else entirely",
                number: 5001,
                ..Default::default()
            },
        );
        add_conversation(
            &conn,
            Conv {
                subject: "Another conversation",
                ..Default::default()
            },
        );
        let hits = search_conversations(&conn, "5001", &SearchFilters::default(), 40).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].title.starts_with("#5001 "));
    }

    #[test]
    fn search_conversation_excludes_deleted_and_merged() {
        let conn = fresh_db();
        let deleted = add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                ..Default::default()
            },
        );
        let merged = add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                ..Default::default()
            },
        );
        add_conversation(
            &conn,
            Conv {
                subject: "billing problem kept",
                ..Default::default()
            },
        );
        conn.execute(
            "UPDATE conversations SET deleted_at = '2025-06-01T00:00:00Z' WHERE id = ?1",
            params![deleted],
        )
        .unwrap();
        conn.execute(
            "UPDATE conversations SET merged_into_conversation_id = ?1 WHERE id = ?2",
            params![deleted, merged],
        )
        .unwrap();
        let hits = search_conversations(&conn, "billing", &SearchFilters::default(), 40).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].title.contains("kept"));
    }

    #[test]
    fn search_conversation_filters_status_mailbox_assignee() {
        let conn = fresh_db();
        let active = add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                status: "active",
                mailbox_id: 1,
                ..Default::default()
            },
        );
        let closed = add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                status: "closed",
                mailbox_id: 2,
                assignee_id: Some(7),
                ..Default::default()
            },
        );

        let hits = search_conversations(
            &conn,
            "billing",
            &SearchFilters {
                status: Some("active".into()),
                ..Default::default()
            },
            40,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, active);

        // status = "all" is not a filter.
        let hits = search_conversations(
            &conn,
            "billing",
            &SearchFilters {
                status: Some("all".into()),
                ..Default::default()
            },
            40,
        )
        .unwrap();
        assert_eq!(hits.len(), 2);

        let hits = search_conversations(
            &conn,
            "billing",
            &SearchFilters {
                mailbox_id: Some(2),
                ..Default::default()
            },
            40,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, closed);

        let hits = search_conversations(
            &conn,
            "billing",
            &SearchFilters {
                assignee_id: Some(7),
                ..Default::default()
            },
            40,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, closed);
    }

    #[test]
    fn search_conversation_filter_tag_case_insensitive() {
        let conn = fresh_db();
        let tagged = add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                ..Default::default()
            },
        );
        add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                ..Default::default()
            },
        );
        conn.execute("INSERT INTO tags (remote_id, name) VALUES (1, 'vip')", [])
            .unwrap();
        let tag_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (?1, ?2)",
            params![tagged, tag_id],
        )
        .unwrap();
        let hits = search_conversations(
            &conn,
            "billing",
            &SearchFilters {
                tag: Some("VIP".into()),
                ..Default::default()
            },
            40,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, tagged);
    }

    #[test]
    fn search_conversation_filter_since_days() {
        let conn = fresh_db();
        add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                created_at: "2000-01-01T00:00:00Z",
                ..Default::default()
            },
        );
        let recent = add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                created_at: "2100-01-01T00:00:00Z",
                ..Default::default()
            },
        );
        let hits = search_conversations(
            &conn,
            "billing",
            &SearchFilters {
                since_days: Some(365),
                ..Default::default()
            },
            40,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, recent);
    }

    #[test]
    fn search_conversation_customer_in_subtitle() {
        let conn = fresh_db();
        let customer = add_customer(&conn, "Alice", "Wonderland", "alice@example.com", None);
        let id = add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                customer_id: customer,
                status: "pending",
                ..Default::default()
            },
        );
        let hits = search_conversations(&conn, "billing", &SearchFilters::default(), 40).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].subtitle,
            "Alice Wonderland · alice@example.com · pending"
        );
        assert_eq!(hits[0].id, id);
    }

    #[test]
    fn empty_query_without_filters_returns_recent_conversations() {
        let conn = fresh_db();
        let older = add_conversation(
            &conn,
            Conv {
                subject: "Older ticket",
                updated_at: "2000-01-01T00:00:00Z",
                ..Default::default()
            },
        );
        let newer = add_conversation(
            &conn,
            Conv {
                subject: "Newer ticket",
                updated_at: "2100-01-01T00:00:00Z",
                ..Default::default()
            },
        );
        let hits = search_conversations(&conn, "", &SearchFilters::default(), 40).unwrap();
        assert_eq!(hits.len(), 2);
        // Ordered by last activity (updated_at) DESC.
        assert_eq!(hits[0].id, newer);
        assert_eq!(hits[1].id, older);
        assert_eq!(hits[0].score, 0.5);
        assert_eq!(hits[0].why, vec!["recent".to_string()]);
    }

    #[test]
    fn empty_query_with_filters_skips_recent_fallback() {
        let conn = fresh_db();
        add_conversation(
            &conn,
            Conv {
                subject: "active ticket",
                status: "active",
                ..Default::default()
            },
        );
        add_conversation(
            &conn,
            Conv {
                subject: "closed ticket",
                status: "closed",
                ..Default::default()
            },
        );
        // Empty query + a real filter: filtered rows, keyword provenance.
        let hits = search_conversations(
            &conn,
            "",
            &SearchFilters {
                status: Some("closed".into()),
                ..Default::default()
            },
            40,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].why, vec!["keyword match".to_string()]);
    }

    // ---- customers ---------------------------------------------------------

    #[test]
    fn search_customers_by_name_and_email() {
        let conn = fresh_db();
        let alice = add_customer(&conn, "Alice", "Wonderland", "alice@example.com", None);
        add_customer(&conn, "Bob", "Jones", "bob@example.com", None);

        let by_name = search_customers(&conn, "Alice", 15).unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].id, alice);
        assert_eq!(by_name[0].scope, "customers");
        assert_eq!(by_name[0].title, "Alice Wonderland");
        assert_eq!(by_name[0].subtitle, "alice@example.com");
        assert_eq!(by_name[0].href, format!("/customers/{alice}"));
        assert_eq!(by_name[0].why, vec!["name/email match".to_string()]);

        let by_email = search_customers(&conn, "alice@example", 15).unwrap();
        assert_eq!(by_email.len(), 1);
        assert_eq!(by_email[0].id, alice);
    }

    #[test]
    fn search_customers_like_wildcards_are_escaped() {
        let conn = fresh_db();
        let literal = add_customer(&conn, "100%sure", "Person", "a@example.com", None);
        add_customer(&conn, "100xsure", "Person", "b@example.com", None);

        // A raw % must not act as a wildcard: only the literal match hits.
        let hits = search_customers(&conn, "100%sure", 15).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, literal);

        let under = add_customer(&conn, "weird_name", "Person", "c@example.com", None);
        add_customer(&conn, "weirdXname", "Person", "d@example.com", None);
        let hits = search_customers(&conn, "weird_name", 15).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, under);
    }

    #[test]
    fn search_customers_org_and_conversation_count_in_snippet() {
        let conn = fresh_db();
        let customer_id = add_customer(
            &conn,
            "Carol",
            "Denver",
            "carol@example.com",
            Some("Acme Corp"),
        );
        add_conversation(
            &conn,
            Conv {
                customer_id,
                ..Default::default()
            },
        );
        add_conversation(
            &conn,
            Conv {
                customer_id,
                ..Default::default()
            },
        );
        let hits = search_customers(&conn, "Carol", 15).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].snippet, "Acme Corp · 2 conversations");
    }

    #[test]
    fn search_customers_empty_query_returns_empty() {
        let conn = fresh_db();
        add_customer(&conn, "Alice", "Wonderland", "alice@example.com", None);
        assert!(search_customers(&conn, "", 15).unwrap().is_empty());
        assert!(search_customers(&conn, "   ", 15).unwrap().is_empty());
    }

    // ---- knowledge ---------------------------------------------------------

    #[test]
    fn search_knowledge_snippet_has_markers_and_visibility_filter() {
        let conn = fresh_db();
        add_knowledge_doc(
            &conn,
            "Refund policy",
            "Customers can request a full refund within thirty days of the original purchase date",
            "customer_safe",
        );
        add_knowledge_doc(
            &conn,
            "Internal refund runbook",
            "Refund approvals above 500 dollars require a manager sign-off",
            "internal_only",
        );

        let raw = search_knowledge_raw(&conn, "refund", None, 10, "and").unwrap();
        assert_eq!(raw.len(), 2);
        assert!(raw
            .iter()
            .all(|r| r.snippet.contains('[') && r.snippet.contains(']')));

        let safe = search_knowledge_raw(&conn, "refund", Some("customer_safe"), 10, "and").unwrap();
        assert_eq!(safe.len(), 1);
        assert_eq!(safe[0].visibility, "customer_safe");
        assert_eq!(safe[0].title, "Refund policy");

        let hits = search_knowledge(&conn, "refund", None, 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.scope == "knowledge"));
        // Rank order between the two docs is not deterministic — compare as a set.
        let mut subtitles: Vec<&str> = hits.iter().map(|h| h.subtitle.as_str()).collect();
        subtitles.sort_unstable();
        assert_eq!(
            subtitles,
            vec!["Knowledge · customer-safe", "Knowledge · internal"]
        );
        assert!(hits
            .iter()
            .all(|h| h.href == format!("/knowledge?doc={}", h.id)));
    }

    #[test]
    fn search_knowledge_or_mode_matches_any_token() {
        let conn = fresh_db();
        add_knowledge_doc(
            &conn,
            "Scheduler quirks",
            "Timezone handling around daylight saving requires care when scheduling",
            "internal_only",
        );
        // AND mode: no doc contains both tokens.
        assert!(
            search_knowledge_raw(&conn, "billing timezone", None, 10, "and")
                .unwrap()
                .is_empty()
        );
        // OR mode: the timezone token matches.
        let raw = search_knowledge_raw(&conn, "billing timezone", None, 10, "or").unwrap();
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].title, "Scheduler quirks");
    }

    // ---- known issues / saved replies / AI ----------------------------------

    #[test]
    fn search_known_issues_snippet_and_href() {
        let conn = fresh_db();
        add_known_issue(
            &conn,
            "Schedules keep previous DST offset",
            "Scheduled reports fire one hour late after a daylight-saving change",
        );
        // The reference snippet renders column 0 (title), so query a title
        // word to see the [ ] markers.
        let hits = search_known_issues(&conn, "schedules", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].scope, "issues");
        assert_eq!(hits[0].title, "Schedules keep previous DST offset");
        assert_eq!(hits[0].subtitle, "Known issue");
        assert_eq!(hits[0].href, "/issues?tab=known");
        assert!(hits[0].snippet.contains('[') && hits[0].snippet.contains(']'));
    }

    #[test]
    fn search_saved_replies_excludes_deleted() {
        let conn = fresh_db();
        add_saved_reply(
            &conn,
            "Refund macro",
            "Please wait five business days",
            "Full text",
        );
        let deleted = add_saved_reply(
            &conn,
            "Refund escalation",
            "Escalate to billing",
            "Full text",
        );
        conn.execute(
            "UPDATE saved_replies SET deleted_at = '2025-06-01T00:00:00Z' WHERE id = ?1",
            params![deleted],
        )
        .unwrap();
        let hits = search_saved_replies(&conn, "refund", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Refund macro");
        assert_eq!(hits[0].scope, "saved_replies");
        assert_eq!(hits[0].subtitle, "Saved reply");
        assert_eq!(hits[0].href, "/inbox");
    }

    #[test]
    fn search_ai_analyses_hit_shape() {
        let conn = fresh_db();
        let id = add_conversation(
            &conn,
            Conv {
                subject: "Invoice question",
                number: 42,
                ..Default::default()
            },
        );
        index_ai_analysis_fts(
            &conn,
            id,
            1,
            Some("Customer asks about a duplicate invoice charge"),
            Some("Why was I billed twice"),
            Some("billing"),
        )
        .unwrap();
        let hits = search_ai_analyses(&conn, "duplicate", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].scope, "ai");
        assert_eq!(hits[0].id, id);
        assert_eq!(hits[0].title, "AI analysis · #42 Invoice question");
        assert_eq!(hits[0].subtitle, "AI-derived (marked as AI-generated)");
        assert_eq!(hits[0].href, format!("/inbox/conversation/{id}"));
        assert!(hits[0].snippet.contains('['));
    }

    // ---- unified search + response shape ------------------------------------

    #[test]
    fn search_all_scopes_returns_mixed_hits() {
        let conn = fresh_db();
        let conv = add_conversation(
            &conn,
            Conv {
                subject: "billing dispute",
                ..Default::default()
            },
        );
        add_thread(&conn, conv, "Please fix the billing error");
        add_customer(&conn, "Bill", "Billingston", "bill@example.com", None);
        add_knowledge_doc(
            &conn,
            "Billing doc",
            "How billing works internally",
            "internal_only",
        );
        add_known_issue(&conn, "Billing outage", "Invoices failed to generate");
        add_saved_reply(&conn, "Billing macro", "Billing explanation text", "Full");
        index_ai_analysis_fts(&conn, conv, 1, Some("billing summary"), None, None).unwrap();

        let resp = search(&conn, "billing", "all", &SearchFilters::default()).unwrap();
        let scopes: Vec<&str> = resp.hits.iter().map(|h| h.scope.as_str()).collect();
        for expected in [
            "tickets",
            "customers",
            "knowledge",
            "issues",
            "saved_replies",
            "ai",
        ] {
            assert!(
                scopes.contains(&expected),
                "missing scope {expected} in {scopes:?}"
            );
        }
        assert_eq!(resp.total, resp.hits.len());
        assert!(!resp.used_semantic);
        assert!(!resp.semantic_available);
    }

    #[test]
    fn search_scope_isolation() {
        let conn = fresh_db();
        let conv = add_conversation(
            &conn,
            Conv {
                subject: "billing dispute",
                ..Default::default()
            },
        );
        add_customer(&conn, "Bill", "Billingston", "bill@example.com", None);

        let tickets = search(&conn, "billing", "tickets", &SearchFilters::default()).unwrap();
        assert!(tickets.hits.iter().all(|h| h.scope == "tickets"));
        assert_eq!(tickets.hits.len(), 1);
        assert_eq!(tickets.hits[0].id, conv);

        let customers = search(&conn, "billing", "customers", &SearchFilters::default()).unwrap();
        assert!(customers.hits.iter().all(|h| h.scope == "customers"));

        let unknown = search(&conn, "billing", "bogus", &SearchFilters::default()).unwrap();
        assert!(unknown.hits.is_empty());

        // Empty scope means "all" (reference default parameter).
        let all = search(&conn, "billing", "", &SearchFilters::default()).unwrap();
        assert!(all.hits.len() >= 2);
    }

    #[test]
    fn search_recent_fallback_through_unified_search() {
        let conn = fresh_db();
        add_conversation(
            &conn,
            Conv {
                subject: "Only ticket",
                ..Default::default()
            },
        );
        let resp = search(&conn, "", "all", &SearchFilters::default()).unwrap();
        assert_eq!(resp.total, 1);
        assert_eq!(resp.hits[0].why, vec!["recent".to_string()]);
    }

    #[test]
    fn search_response_serializes_reference_shape() {
        let conn = fresh_db();
        add_conversation(
            &conn,
            Conv {
                subject: "Billing issue",
                ..Default::default()
            },
        );
        let resp = search(&conn, "billing", "all", &SearchFilters::default()).unwrap();
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["query"], "billing");
        assert_eq!(v["total"], 1);
        assert_eq!(v["used_semantic"], false);
        assert_eq!(v["semantic_available"], false);
        assert!(v.get("mode_note").is_none());
        let hit = &v["hits"][0];
        assert_eq!(hit["scope"], "tickets");
        assert_eq!(hit["score"], 1); // integer, like the reference
        assert_eq!(hit["why"][0], "keyword match");
        assert!(hit["href"]
            .as_str()
            .unwrap()
            .starts_with("/inbox/conversation/"));
    }

    // ---- rebuild -------------------------------------------------------------

    #[test]
    fn rebuild_indexes_repopulates_all_fts_tables() {
        let conn = fresh_db();
        let conv = add_conversation(
            &conn,
            Conv {
                subject: "invoice billing problem",
                ..Default::default()
            },
        );
        add_thread(&conn, conv, "The billing invoice needs a correction");
        add_customer(&conn, "Bill", "Ing", "bill@example.com", None);
        add_knowledge_doc(
            &conn,
            "Billing doc",
            "How billing invoices work",
            "internal_only",
        );
        add_known_issue(
            &conn,
            "Billing outage",
            "Invoices failed to generate after billing",
        );
        add_saved_reply(&conn, "Billing macro", "Billing explanation", "Full text");
        let run_id = add_ai_run(&conn);
        conn.execute(
            "INSERT INTO ai_extracted_facts (conversation_id, run_id, key, value)
             VALUES (?1, ?2, 'summary', 'billing invoice summary')",
            params![conv, run_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO docs_articles (remote_id, name, text) VALUES (1, 'Billing article', 'How billing works')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO custom_object_types (name, slug) VALUES ('Rollout', 'rollout')",
            [],
        )
        .unwrap();
        let type_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO custom_objects (type_id, title, data_json) VALUES (?1, 'Acme rollout', ?2)",
            params![type_id, r#"{"priority":"urgent","seats":42}"#],
        )
        .unwrap();

        // Wipe every FTS table — search goes dark.
        for table in [
            "fts_conversations",
            "fts_threads",
            "fts_knowledge",
            "fts_known_issues",
            "fts_saved_replies",
            "fts_ai_analyses",
            "docs_fts",
            "fts_custom_objects",
        ] {
            conn.execute(&format!("DELETE FROM {table}"), []).unwrap();
        }
        assert!(
            search_conversations(&conn, "billing", &SearchFilters::default(), 40)
                .unwrap()
                .is_empty()
        );
        assert!(search_knowledge(&conn, "billing", None, 10)
            .unwrap()
            .is_empty());

        // Rebuild restores everything.
        let n = rebuild_indexes(&conn).unwrap();
        assert_eq!(n, 1);
        assert!(
            !search_conversations(&conn, "billing", &SearchFilters::default(), 40)
                .unwrap()
                .is_empty()
        );
        assert!(!search_knowledge(&conn, "billing", None, 10)
            .unwrap()
            .is_empty());
        assert!(!search_known_issues(&conn, "billing", 10)
            .unwrap()
            .is_empty());
        assert!(!search_saved_replies(&conn, "billing", 10)
            .unwrap()
            .is_empty());
        assert!(!search_ai_analyses(&conn, "billing", 10).unwrap().is_empty());

        let docs_hit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM docs_fts WHERE docs_fts MATCH '\"billing\"*'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(docs_hit, 1);

        let custom_hit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fts_custom_objects WHERE fts_custom_objects MATCH '\"urgent\"*'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(custom_hit, 1);

        // Threads marked indexed.
        let flag: i64 = conn
            .query_row("SELECT fts_indexed FROM conversation_threads", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(flag, 1);

        // Index version recorded.
        let version: String = conn
            .query_row(
                "SELECT value FROM application_settings WHERE key = 'fts_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "3");
    }

    #[test]
    fn rebuild_indexes_is_idempotent() {
        let conn = fresh_db();
        add_conversation(
            &conn,
            Conv {
                subject: "billing problem",
                ..Default::default()
            },
        );
        let first = rebuild_indexes(&conn).unwrap();
        let counts: Vec<(String, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT 'fts_conversations', COUNT(*) FROM fts_conversations
                          UNION ALL SELECT 'fts_threads', COUNT(*) FROM fts_threads
                          UNION ALL SELECT 'fts_knowledge', COUNT(*) FROM fts_knowledge
                          UNION ALL SELECT 'fts_known_issues', COUNT(*) FROM fts_known_issues
                          UNION ALL SELECT 'fts_saved_replies', COUNT(*) FROM fts_saved_replies
                          UNION ALL SELECT 'fts_ai_analyses', COUNT(*) FROM fts_ai_analyses",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        let second = rebuild_indexes(&conn).unwrap();
        assert_eq!(first, second);
        let counts_after: Vec<(String, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT 'fts_conversations', COUNT(*) FROM fts_conversations
                          UNION ALL SELECT 'fts_threads', COUNT(*) FROM fts_threads
                          UNION ALL SELECT 'fts_knowledge', COUNT(*) FROM fts_knowledge
                          UNION ALL SELECT 'fts_known_issues', COUNT(*) FROM fts_known_issues
                          UNION ALL SELECT 'fts_saved_replies', COUNT(*) FROM fts_saved_replies
                          UNION ALL SELECT 'fts_ai_analyses', COUNT(*) FROM fts_ai_analyses",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert_eq!(counts, counts_after);
    }

    #[test]
    fn rebuild_backfills_ai_analyses_from_extracted_facts() {
        let conn = fresh_db();
        let conv = add_conversation(
            &conn,
            Conv {
                subject: "Analysis subject",
                ..Default::default()
            },
        );
        let run_id = add_ai_run(&conn);
        conn.execute(
            "INSERT INTO ai_extracted_facts (conversation_id, run_id, key, value) VALUES (?1, ?2, 'summary', 'duplicate invoice seen twice')",
            params![conv, run_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ai_extracted_facts (conversation_id, run_id, key, value) VALUES (?1, ?2, 'intent', 'billing')",
            params![conv, run_id],
        )
        .unwrap();
        // Irrelevant key: not indexed.
        conn.execute(
            "INSERT INTO ai_extracted_facts (conversation_id, run_id, key, value) VALUES (?1, ?2, 'product', 'Reports')",
            params![conv, run_id],
        )
        .unwrap();
        assert!(search_ai_analyses(&conn, "duplicate", 10)
            .unwrap()
            .is_empty());
        rebuild_indexes(&conn).unwrap();
        let hits = search_ai_analyses(&conn, "duplicate", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, conv);
    }

    // ---- legacy compatibility -------------------------------------------------

    #[test]
    fn legacy_index_conversation_feeds_reference_engine() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_local_id, customer_local_id, subject, preview)
             VALUES (1001, 1001, 'active', 101, 2001, 'Billing issue', 'wrong amount')",
            [],
        )
        .unwrap();
        // Legacy caller keys by remote_id (perf dataset path).
        index_conversation(&conn, 1001, "Billing issue", "wrong amount").unwrap();

        let legacy: Vec<SearchResult> = universal_search(&conn, "billing").unwrap();
        assert!(!legacy.is_empty());
        assert_eq!(legacy[0].resource_type, "tickets");

        let hits = search_conversations(&conn, "billing", &SearchFilters::default(), 40).unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn universal_search_preserves_legacy_contract() {
        let conn = fresh_db();
        add_conversation(
            &conn,
            Conv {
                subject: "Refund request",
                ..Default::default()
            },
        );
        let results = universal_search(&conn, "refund").unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().all(|r| r.title.contains("Refund")));

        // Empty query → empty result (legacy hardening).
        assert!(universal_search(&conn, "").unwrap().is_empty());
        assert!(universal_search(&conn, "   ").unwrap().is_empty());

        // Long query is capped, not an error.
        let long_query = "refund ".repeat(200);
        assert!(universal_search(&conn, &long_query).is_ok());
    }

    #[test]
    fn apply_fts_migration_is_idempotent() {
        let conn = fresh_db();
        apply_fts_migration(&conn).unwrap();
        apply_fts_migration(&conn).unwrap();
    }

    #[test]
    fn search_works_on_minimal_schema() {
        // The hybrid_search/perf_guards style DB: base migrations + FTS only.
        let conn = minimal_db();
        let resp = search(&conn, "anything", "all", &SearchFilters::default()).unwrap();
        assert!(resp.hits.is_empty());
        assert_eq!(resp.total, 0);

        // Recent fallback is safe too.
        let recent = search(&conn, "", "all", &SearchFilters::default()).unwrap();
        assert_eq!(recent.total, 0);

        // Rebuild works and records the version.
        let n = rebuild_indexes(&conn).unwrap();
        assert_eq!(n, 0);
        let version: String = conn
            .query_row(
                "SELECT value FROM application_settings WHERE key = 'fts_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "3");
    }
}
