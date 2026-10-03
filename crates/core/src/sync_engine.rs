//! SyncCoordinator — maintains the local mirror of Help Scout data.
//!
//! Direct port of the reference `src/server/sync/coordinator.ts`:
//! - dependency-aware INITIAL_SYNC_ORDER (spec #10)
//! - incremental sync with a 10-minute overlap window (spec #11)
//! - reconciliation for merged/deleted/missing records (spec #12)
//! - checkpoints make every step resumable after restart
//! - cancellation is cooperative (current resource finishes first)
//! - SSE `sync` event emitted on completion (serverEventBus parity)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::error::{Error, Result};

use crate::helpscout::{
    ConversationQuery, CustomerQuery, HelpScoutProvider, HsConversation, HsThread,
};
use crate::sync as mirror;

/// Dependency-aware initial sync order (reference INITIAL_SYNC_ORDER).
pub const INITIAL_SYNC_ORDER: [&str; 22] = [
    "account",
    "users",
    "system_users",
    "teams",
    "mailboxes",
    "folders",
    "tags",
    "inbox_fields",
    "customer_property_definitions",
    "organization_property_definitions",
    "organizations", // before customers: customer rows reference their organization
    "customers",
    "saved_replies",
    "workflows",
    "conversations",
    "threads",
    "chats", // Beacon chat sessions - channel catch-up after conversation sync
    "attachments",
    "ratings",
    "docs_collections", // Docs API mirror (docsapi.helpscout.net, separate API key)
    "docs_articles",
    "user_statuses",
];

/// Incremental reference-data refresh order (reference incrementalSync step 1).
pub const INCREMENTAL_REFERENCE_ORDER: [&str; 13] = [
    "mailboxes",
    "folders",
    "tags",
    "users",
    "teams",
    "workflows",
    "saved_replies",
    "inbox_fields",
    "customer_property_definitions",
    "organization_property_definitions",
    "organizations",
    "docs_collections",
    "docs_articles",
];

/// Overlap window for incremental sync watermarks (minutes).
pub const SYNC_OVERLAP_MINUTES: i64 = 10;

/// Per-resource sync outcome (reference ResourceSyncResult).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResourceSyncResult {
    pub resource: String,
    pub processed: i64,
    pub failed: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Reconciliation outcome (reference ReconciliationResult).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReconciliationResult {
    pub checked: i64,
    pub added: i64,
    pub updated: i64,
    pub deleted: i64,
    pub merged: i64,
    pub failed: i64,
    pub skipped: i64,
    pub details: Vec<String>,
}

// ---------------------------------------------------------------------------
// SyncRepository helpers (syncRepo.ts parity)
// ---------------------------------------------------------------------------

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// `getState()` — sync_state from application_settings ('NEW' when unset).
/// The reference stores `JSON.stringify(state)` (a quoted string), so the
/// read side parses the JSON wrapping.
pub fn get_state(conn: &Connection) -> String {
    conn.query_row(
        "SELECT value FROM application_settings WHERE key = 'sync_state'",
        [],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| serde_json::from_str::<String>(&v).ok().or(Some(v)))
    .unwrap_or_else(|| "NEW".to_string())
}

/// `setState(state)`.
pub fn set_state(conn: &Connection, state: &str) {
    let _ = conn.execute(
        "INSERT INTO application_settings (key, value, updated_at)
         VALUES ('sync_state', ?1, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![format!("\"{state}\"")],
    );
}

/// `startRun(kind)` — insert a sync_runs row, return its id.
pub fn start_run(conn: &Connection, kind: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO sync_runs (kind, state, started_at) VALUES (?1, 'INITIALIZING', ?2)",
        params![kind, now_iso()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// `updateRun(id, {state, resources_done, resources_total, records_processed,
/// errors, detail, finished})`.
pub struct RunUpdate<'a> {
    pub state: Option<&'a str>,
    pub resources_done: Option<i64>,
    pub resources_total: Option<i64>,
    pub records_processed: Option<i64>,
    pub errors: Option<i64>,
    pub detail: Option<Value>,
    pub finished: bool,
}

pub fn update_run(conn: &Connection, id: i64, fields: RunUpdate<'_>) -> Result<()> {
    let mut sets: Vec<&str> = Vec::new();
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(s) = fields.state {
        sets.push("state = ?");
        args.push(Box::new(s.to_string()));
    }
    if let Some(v) = fields.resources_done {
        sets.push("resources_done = ?");
        args.push(Box::new(v));
    }
    if let Some(v) = fields.resources_total {
        sets.push("resources_total = ?");
        args.push(Box::new(v));
    }
    if let Some(v) = fields.records_processed {
        sets.push("records_processed = ?");
        args.push(Box::new(v));
    }
    if let Some(v) = fields.errors {
        sets.push("errors = ?");
        args.push(Box::new(v));
    }
    if let Some(v) = fields.detail {
        sets.push("detail = ?");
        args.push(Box::new(v.to_string()));
    }
    if fields.finished {
        sets.push("finished_at = datetime('now')");
    }
    if sets.is_empty() {
        return Ok(());
    }
    let sql = format!("UPDATE sync_runs SET {} WHERE id = ?", sets.join(", "));
    args.push(Box::new(id));
    let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    conn.execute(&sql, refs.as_slice())?;
    Ok(())
}

/// `recordSuccess(resource, recordsProcessed)`.
pub fn record_success(conn: &Connection, resource: &str, records_processed: i64) {
    let _ = conn.execute(
        "INSERT INTO sync_checkpoints (resource, last_success_at, records_processed, records_failed, last_error, retry_count, status)
         VALUES (?1, ?2, ?3, 0, NULL, 0, 'ok')
         ON CONFLICT(resource) DO UPDATE SET last_success_at = excluded.last_success_at,
           records_processed = records_processed + excluded.records_processed,
           last_error = NULL, retry_count = 0, status = 'ok'",
        params![resource, now_iso(), records_processed],
    );
}

/// `recordFailure(resource, error, recordsFailed)`.
pub fn record_failure(conn: &Connection, resource: &str, error: &str, records_failed: i64) {
    let clipped: String = error.chars().take(500).collect();
    let _ = conn.execute(
        "INSERT INTO sync_checkpoints (resource, records_processed, records_failed, last_error, retry_count, status)
         VALUES (?1, 0, ?2, ?3, 1, 'error')
         ON CONFLICT(resource) DO UPDATE SET records_failed = records_failed + excluded.records_failed,
           last_error = excluded.last_error, retry_count = retry_count + 1, status = 'error'",
        params![resource, records_failed, clipped],
    );
}

/// `setCheckpointRunning(resource, running)`.
pub fn set_checkpoint_running(conn: &Connection, resource: &str, running: bool) {
    let _ = conn.execute(
        "INSERT INTO sync_checkpoints (resource, status) VALUES (?1, ?2)
         ON CONFLICT(resource) DO UPDATE SET status = excluded.status",
        params![resource, if running { "running" } else { "idle" }],
    );
}

/// `getIncrementalSince(resource, overlapMinutes)` — the watermark.
pub fn get_incremental_since(
    conn: &Connection,
    resource: &str,
    overlap_minutes: i64,
) -> Option<String> {
    let last: Option<String> = conn
        .query_row(
            "SELECT last_success_at FROM sync_checkpoints WHERE resource = ?1",
            params![resource],
            |r| r.get(0),
        )
        .ok()
        .flatten()?;
    let t = chrono::DateTime::parse_from_rfc3339(last.as_deref().unwrap_or_default()).ok()?;
    let shifted = t - chrono::Duration::minutes(overlap_minutes);
    Some(shifted.to_rfc3339())
}

/// `lastSuccessfulSync()` — MAX(last_success_at) over ok checkpoints.
pub fn last_successful_sync(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT MAX(last_success_at) FROM sync_checkpoints WHERE status = 'ok'",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// Resolve a local id from a remote id (`ref.getLocalId` parity).
pub fn local_id(conn: &Connection, table: &str, remote_id: i64) -> Option<i64> {
    conn.query_row(
        &format!("SELECT id FROM {table} WHERE remote_id = ?1"),
        params![remote_id],
        |r| r.get(0),
    )
    .ok()
}

/// `getConversationByRemoteId`.
pub fn conversation_local_id(conn: &Connection, remote_id: i64) -> Option<i64> {
    conn.query_row(
        "SELECT id FROM conversations WHERE remote_id = ?1",
        params![remote_id],
        |r| r.get(0),
    )
    .ok()
}

// ---------------------------------------------------------------------------
// Mirror upserts for the M029 resource tables
// ---------------------------------------------------------------------------

fn upsert_folder(
    conn: &Connection,
    mailbox_local: i64,
    f: &crate::helpscout::HsFolder,
) -> Result<()> {
    conn.execute(
        "INSERT INTO folders (remote_id, mailbox_id, name, type, user_id, total_count, active_count, last_synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
         ON CONFLICT(remote_id) DO UPDATE SET
            mailbox_id = excluded.mailbox_id, name = excluded.name, type = excluded.type,
            user_id = excluded.user_id, total_count = excluded.total_count,
            active_count = excluded.active_count, last_synced_at = datetime('now')",
        params![f.remote_id, mailbox_local, f.name, f.kind, f.user_id, f.total_count, f.active_count],
    )?;
    Ok(())
}

fn upsert_inbox_field(
    conn: &Connection,
    mailbox_local: i64,
    f: &crate::helpscout::HsField,
) -> Result<()> {
    conn.execute(
        "INSERT INTO inbox_fields (remote_id, mailbox_id, name, type, system_type, required, sort_order, last_synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
         ON CONFLICT(remote_id) DO UPDATE SET
            mailbox_id = excluded.mailbox_id, name = excluded.name, type = excluded.type,
            system_type = excluded.system_type, required = excluded.required,
            sort_order = excluded.sort_order, last_synced_at = datetime('now')",
        params![f.remote_id, mailbox_local, f.name, f.kind, f.system_type, i64::from(f.required), f.sort_order],
    )?;
    for opt in &f.options {
        conn.execute(
            "INSERT INTO inbox_field_options (remote_id, field_id, label, sort_order)
             VALUES (?1, (SELECT id FROM inbox_fields WHERE remote_id = ?2), ?3, ?4)
             ON CONFLICT(remote_id) DO UPDATE SET label = excluded.label, sort_order = excluded.sort_order",
            params![opt.id, f.remote_id, opt.label, opt.order],
        )?;
    }
    Ok(())
}

fn upsert_property_definitions(
    conn: &Connection,
    table: &str,
    defs: &[crate::helpscout::HsPropertyDef],
) -> Result<()> {
    for d in defs {
        conn.execute(
            &format!(
                "INSERT INTO {table} (remote_id, name, slug, type, sort_order, last_synced_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
                 ON CONFLICT(remote_id) DO UPDATE SET name = excluded.name, slug = excluded.slug,
                   type = excluded.type, sort_order = excluded.sort_order, last_synced_at = datetime('now')"
            ),
            params![d.remote_id, d.name, d.slug, d.kind, d.sort_order],
        )?;
    }
    Ok(())
}

pub fn upsert_organization(conn: &Connection, o: &crate::helpscout::HsOrganization) -> Result<()> {
    let domains = serde_json::to_string(&o.domains).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO organizations (remote_id, name, domains, remote_created_at, remote_updated_at, last_seen_at, last_synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), datetime('now'))
         ON CONFLICT(remote_id) DO UPDATE SET name = excluded.name, domains = excluded.domains,
           remote_created_at = excluded.remote_created_at, remote_updated_at = excluded.remote_updated_at,
           last_seen_at = datetime('now'), last_synced_at = datetime('now')",
        params![o.remote_id, o.name, domains, o.created_at, o.updated_at],
    )?;
    Ok(())
}

fn upsert_saved_reply(
    conn: &Connection,
    mailbox_local: Option<i64>,
    r: &crate::helpscout::HsSavedReply,
) -> Result<()> {
    conn.execute(
        "INSERT INTO saved_replies (remote_id, mailbox_local_id, name, preview, text, last_synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
         ON CONFLICT(remote_id) DO UPDATE SET mailbox_local_id = excluded.mailbox_local_id,
           name = excluded.name, preview = excluded.preview, text = excluded.text,
           last_synced_at = datetime('now')",
        params![r.remote_id, mailbox_local, r.name, r.preview, r.text],
    )?;
    Ok(())
}

fn upsert_workflow(conn: &Connection, w: &crate::helpscout::HsWorkflow) -> Result<()> {
    let mailbox_local = w.mailbox_id.and_then(|m| local_id(conn, "mailboxes", m));
    conn.execute(
        "INSERT INTO workflows (remote_id, mailbox_local_id, name, type, status, sort_order, last_synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(remote_id) DO UPDATE SET mailbox_local_id = excluded.mailbox_local_id,
           name = excluded.name, type = excluded.type, status = excluded.status,
           sort_order = excluded.sort_order, last_synced_at = datetime('now')",
        params![w.remote_id, mailbox_local, w.name, w.kind, w.status, w.sort_order],
    )?;
    Ok(())
}

fn upsert_user_status(
    conn: &Connection,
    user_local: i64,
    s: &crate::helpscout::HsUserStatus,
) -> Result<()> {
    conn.execute(
        "INSERT INTO user_statuses (user_local_id, email_status, email_updated_at, chat_status, mailbox_statuses, last_synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
         ON CONFLICT(user_local_id) DO UPDATE SET email_status = excluded.email_status,
           email_updated_at = excluded.email_updated_at, chat_status = excluded.chat_status,
           mailbox_statuses = excluded.mailbox_statuses, last_synced_at = datetime('now')",
        params![user_local, s.email_status, s.email_updated_at, s.chat_status, s.mailbox_statuses.to_string()],
    )?;
    Ok(())
}

fn upsert_doc_collection(conn: &Connection, c: &crate::helpscout::HsDocCollection) -> Result<i64> {
    conn.execute(
        "INSERT INTO docs_collections (remote_id, slug, name, last_synced_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(remote_id) DO UPDATE SET slug = excluded.slug, name = excluded.name,
           last_synced_at = datetime('now')",
        params![c.remote_id, c.slug, c.name],
    )?;
    local_id(conn, "docs_collections", c.remote_id)
        .ok_or_else(|| Error::Other("docs_collections upsert lost its row".into()))
}

fn upsert_doc_categories(
    conn: &Connection,
    collection_local: i64,
    cats: &[crate::helpscout::HsDocCategory],
) -> Result<()> {
    for c in cats {
        conn.execute(
            "INSERT INTO docs_categories (remote_id, collection_local_id, slug, name, last_synced_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(remote_id) DO UPDATE SET collection_local_id = excluded.collection_local_id,
               slug = excluded.slug, name = excluded.name, last_synced_at = datetime('now')",
            params![c.remote_id, collection_local, c.slug, c.name],
        )?;
    }
    Ok(())
}

fn upsert_doc_article(
    conn: &Connection,
    a: &crate::helpscout::HsDocArticle,
    collection_local: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO docs (remote_id, collection_local_id, slug, name, text, remote_created_at, remote_updated_at, last_synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
         ON CONFLICT(remote_id) DO UPDATE SET collection_local_id = excluded.collection_local_id,
           slug = excluded.slug, name = excluded.name, text = excluded.text,
           remote_created_at = excluded.remote_created_at, remote_updated_at = excluded.remote_updated_at,
           last_synced_at = datetime('now')",
        params![a.remote_id, collection_local, a.slug, a.name, a.text, a.created_at, a.updated_at],
    )?;
    Ok(())
}

fn upsert_thread(conn: &Connection, conversation_local: i64, t: &HsThread) -> Result<()> {
    let (actor_type, actor_id) = if let Some(cid) = t.created_by_customer_id {
        ("customer", cid)
    } else if let Some(uid) = t.created_by_user_id {
        ("user", uid)
    } else {
        ("system", 0)
    };
    conn.execute(
        "INSERT INTO conversation_threads (conversation_id, thread_type, body, actor_type, actor_id, created_at, remote_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(remote_id) DO UPDATE SET
            conversation_id = excluded.conversation_id, thread_type = excluded.thread_type,
            body = excluded.body, actor_type = excluded.actor_type, actor_id = excluded.actor_id,
            created_at = excluded.created_at",
        params![conversation_local, t.kind, t.body, actor_type, actor_id, t.created_at, t.remote_id],
    )?;
    Ok(())
}

/// Whether the stored conversation row already reflects the remote's
/// userUpdatedAt — the reference's reconcile skip condition (the port's
/// `updated_at` column mirrors the remote userUpdatedAt).
fn stored_user_updated_at_matches(
    conn: &Connection,
    remote_id: i64,
    remote_updated_at: Option<&str>,
) -> bool {
    let Some(remote) = remote_updated_at else {
        return false;
    };
    conn.query_row(
        "SELECT updated_at FROM conversations WHERE remote_id = ?1",
        params![remote_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .is_some_and(|stored| stored == remote)
}

/// Soft-delete a conversation by remote id. Sets BOTH markers: the
/// reference-shaped `deleted_at` timestamp and the port's historical
/// `status = 'deleted'` overload (kept for older read paths).
fn soft_delete_by_remote_id(conn: &Connection, remote_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET status = 'deleted',
             deleted_at = COALESCE(deleted_at, datetime('now'))
           WHERE remote_id = ?1",
        params![remote_id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The coordinator
// ---------------------------------------------------------------------------

/// The sync coordinator (reference SyncCoordinator).
pub struct SyncEngine {
    conn: Arc<Mutex<Connection>>,
    provider: Arc<dyn HelpScoutProvider>,
    running: AtomicBool,
    cancel_requested: AtomicBool,
    /// SSE bus for the `sync` completion event.
    pub bus: Option<crate::http::EventBus>,
}

impl SyncEngine {
    #[must_use]
    pub fn new(conn: Arc<Mutex<Connection>>, provider: Arc<dyn HelpScoutProvider>) -> Self {
        Self {
            conn,
            provider,
            running: AtomicBool::new(false),
            cancel_requested: AtomicBool::new(false),
            bus: None,
        }
    }

    #[must_use]
    pub fn with_bus(mut self, bus: crate::http::EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Whether a sync is currently running (coordinator.running).
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// `requestCancellation()` — the current resource finishes, then stops.
    pub fn request_cancellation(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    fn cancelled(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit_sync_completed(&self, kind: &str, processed: i64, errors: i64) {
        if let Some(bus) = &self.bus {
            use crate::events::ServerEvent;
            bus.emit(&ServerEvent::sync_completed(
                kind,
                processed.max(0) as u32,
                errors.max(0) as u32,
            ));
        }
    }

    // =================================================================
    // INITIAL SYNC — dependency-aware order (spec #10)
    // =================================================================
    pub async fn initial_sync(&self) -> Result<Vec<ResourceSyncResult>> {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(Error::Other("A sync is already running".into()));
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        let run_id = {
            let conn = self.lock();
            set_state(&conn, "INITIALIZING");
            start_run(&conn, "initial")?
        };
        let mut results: Vec<ResourceSyncResult> = Vec::new();
        let outcome: Result<()> = async {
            for resource in INITIAL_SYNC_ORDER {
                if self.cancelled() {
                    break;
                }
                {
                    let conn = self.lock();
                    set_checkpoint_running(&conn, resource, true);
                    let _ = update_run(
                        &conn,
                        run_id,
                        RunUpdate {
                            state: None,
                            resources_done: Some(results.len() as i64),
                            resources_total: Some(INITIAL_SYNC_ORDER.len() as i64),
                            records_processed: None,
                            errors: None,
                            detail: None,
                            finished: false,
                        },
                    );
                }
                let r = self.sync_resource(resource, true).await;
                match &r {
                    Ok(res) if res.error.is_some() => {
                        let conn = self.lock();
                        record_failure(
                            &conn,
                            resource,
                            res.error.as_deref().unwrap_or(""),
                            res.failed,
                        );
                    }
                    Ok(res) => {
                        let conn = self.lock();
                        record_success(&conn, resource, res.processed);
                    }
                    Err(e) => {
                        let conn = self.lock();
                        record_failure(&conn, resource, &e.to_string(), 1);
                    }
                }
                let r = match r {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!(resource, error = %e, "sync resource failed");
                        ResourceSyncResult {
                            resource: resource.to_string(),
                            processed: 0,
                            failed: 1,
                            error: Some(e.to_string()),
                        }
                    }
                };
                let processed: i64 = results.iter().map(|x| x.processed).sum::<i64>() + r.processed;
                let errors = results.iter().filter(|x| x.error.is_some()).count() as i64
                    + i64::from(r.error.is_some());
                results.push(r);
                {
                    let conn = self.lock();
                    let _ = update_run(
                        &conn,
                        run_id,
                        RunUpdate {
                            state: None,
                            resources_done: Some(results.len() as i64),
                            resources_total: None,
                            records_processed: Some(processed),
                            errors: Some(errors),
                            detail: None,
                            finished: false,
                        },
                    );
                }
            }
            let failed = results.iter().filter(|r| r.error.is_some()).count() as i64;
            {
                let conn = self.lock();
                let new_state = if failed == 0 {
                    "LIVE"
                } else if failed < results.len() as i64 / 2 {
                    "CATCHING_UP"
                } else {
                    "ERROR"
                };
                set_state(&conn, new_state);
                let _ = update_run(
                    &conn,
                    run_id,
                    RunUpdate {
                        state: Some(if failed == 0 { "LIVE" } else { "ERROR" }),
                        resources_done: None,
                        resources_total: None,
                        records_processed: None,
                        errors: None,
                        detail: None,
                        finished: true,
                    },
                );
            }
            let processed: i64 = results.iter().map(|r| r.processed).sum();
            self.emit_sync_completed("initial", processed, failed);
            Ok(())
        }
        .await;
        if let Err(e) = outcome {
            let conn = self.lock();
            set_state(&conn, "ERROR");
            let _ = update_run(
                &conn,
                run_id,
                RunUpdate {
                    state: Some("ERROR"),
                    resources_done: None,
                    resources_total: None,
                    records_processed: None,
                    errors: None,
                    detail: Some(json!({ "error": e.to_string() })),
                    finished: true,
                },
            );
            let _ = crate::jobs::log_error(&conn, "sync", &format!("Initial sync failed: {e}"));
        }
        self.running.store(false, Ordering::SeqCst);
        Ok(results)
    }

    // =================================================================
    // INCREMENTAL SYNC (spec #11)
    // =================================================================
    pub async fn incremental_sync(&self) -> Result<Vec<ResourceSyncResult>> {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(Error::Other("A sync is already running".into()));
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        let run_id = {
            let conn = self.lock();
            if get_state(&conn) != "LIVE" {
                set_state(&conn, "CATCHING_UP");
            }
            start_run(&conn, "incremental")?
        };
        let mut results: Vec<ResourceSyncResult> = Vec::new();
        let outcome: Result<()> = async {
            // 1. Reference data refresh (cheap) + docs mirror.
            for resource in INCREMENTAL_REFERENCE_ORDER {
                if self.cancelled() {
                    break;
                }
                let r = self.sync_resource(resource, false).await;
                let r = self.finish_resource(resource, r, &mut results);
                let _ = r;
            }
            // 2. Customers changed since checkpoint.
            {
                let since = {
                    let conn = self.lock();
                    get_incremental_since(&conn, "customers", SYNC_OVERLAP_MINUTES)
                };
                let r = self.sync_customers(since.as_deref()).await;
                let _ = self.finish_resource("customers", r, &mut results);
            }
            // 3. Conversations changed since checkpoint (overlap window).
            {
                let since = {
                    let conn = self.lock();
                    get_incremental_since(&conn, "conversations", SYNC_OVERLAP_MINUTES)
                };
                let r = self.sync_conversations(since.as_deref(), false).await;
                let _ = self.finish_resource("conversations", r, &mut results);
            }
            let failed = results.iter().filter(|r| r.error.is_some()).count() as i64;
            {
                let conn = self.lock();
                set_state(&conn, if failed == 0 { "LIVE" } else { "CATCHING_UP" });
                let processed: i64 = results.iter().map(|r| r.processed).sum();
                let _ = update_run(
                    &conn,
                    run_id,
                    RunUpdate {
                        state: Some(if failed == 0 { "LIVE" } else { "CATCHING_UP" }),
                        resources_done: None,
                        resources_total: None,
                        records_processed: Some(processed),
                        errors: Some(failed),
                        detail: None,
                        finished: true,
                    },
                );
            }
            let processed: i64 = results.iter().map(|r| r.processed).sum();
            self.emit_sync_completed("incremental", processed, failed);
            Ok(())
        }
        .await;
        if let Err(e) = outcome {
            let conn = self.lock();
            set_state(&conn, "ERROR");
            let _ = update_run(
                &conn,
                run_id,
                RunUpdate {
                    state: Some("ERROR"),
                    resources_done: None,
                    resources_total: None,
                    records_processed: None,
                    errors: None,
                    detail: Some(json!({ "error": e.to_string() })),
                    finished: true,
                },
            );
            let _ = crate::jobs::log_error(&conn, "sync", &format!("Incremental sync failed: {e}"));
        }
        self.running.store(false, Ordering::SeqCst);
        Ok(results)
    }

    /// Shared post-resource bookkeeping (checkpoint + results vec).
    fn finish_resource(
        &self,
        resource: &str,
        r: Result<ResourceSyncResult>,
        results: &mut Vec<ResourceSyncResult>,
    ) -> ResourceSyncResult {
        match &r {
            Ok(res) if res.error.is_some() => {
                let conn = self.lock();
                record_failure(
                    &conn,
                    resource,
                    res.error.as_deref().unwrap_or(""),
                    res.failed,
                );
            }
            Ok(res) => {
                let conn = self.lock();
                record_success(&conn, resource, res.processed);
            }
            Err(e) => {
                let conn = self.lock();
                record_failure(&conn, resource, &e.to_string(), 1);
            }
        }
        let r = r.unwrap_or(ResourceSyncResult {
            resource: resource.to_string(),
            processed: 0,
            failed: 1,
            error: Some("resource sync failed".into()),
        });
        results.push(r.clone());
        r
    }

    // =================================================================
    // RECONCILIATION (spec #12)
    // =================================================================
    pub async fn reconcile(&self) -> Result<ReconciliationResult> {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(Error::Other("A sync is already running".into()));
        }
        let run_id = {
            let conn = self.lock();
            set_state(&conn, "RECONCILING");
            start_run(&conn, "reconciliation")?
        };
        let mut result = ReconciliationResult::default();
        let outcome: Result<()> = async {
            // Phase 1: full remote listing — find missing local records.
            // Unchanged conversations are skipped (the reference compares
            // the stored userUpdatedAt against the remote one).
            let mut remote_ids: Vec<i64> = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let page = self
                    .provider
                    .list_conversations(&ConversationQuery {
                        status: Some("all".into()),
                        cursor: cursor.clone(),
                        ..Default::default()
                    })
                    .await?;
                for c in &page.items {
                    remote_ids.push(c.remote_id);
                    let existing = {
                        let conn = self.lock();
                        conversation_local_id(&conn, c.remote_id)
                    };
                    match existing {
                        None => {
                            self.ingest_conversation(c, &[], true).await?;
                            result.added += 1;
                            result
                                .details
                                .push(format!("Added missing conversation #{}", c.number));
                        }
                        Some(_) => {
                            let unchanged = {
                                let conn = self.lock();
                                stored_user_updated_at_matches(&conn, c.remote_id, c.updated_at.as_deref())
                            };
                            if unchanged {
                                result.skipped += 1;
                            } else {
                                self.ingest_conversation(c, &[], true).await?;
                                result.updated += 1;
                            }
                        }
                    }
                    result.checked += 1;
                }
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }

            // Phase 2: local records missing remotely (merged / deleted).
            let locals: Vec<(i64, i64, i64)> = {
                let conn = self.lock();
                let mut stmt = conn.prepare(
                    "SELECT id, remote_id, number FROM conversations WHERE deleted_at IS NULL",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
                })?;
                rows.filter_map(|r| r.ok()).collect()
            };
            for (local_id, remote_id, number) in locals {
                result.checked += 1;
                if remote_ids.contains(&remote_id) {
                    result.skipped += 1;
                    continue;
                }
                match self.provider.get_conversation(remote_id).await {
                    Ok(Some(remote)) => {
                        self.ingest_conversation(&remote, &[], true).await?;
                        result.updated += 1;
                        result
                            .details
                            .push(format!("Refreshed stale conversation #{number}"));
                    }
                    Ok(None) => {
                        let conn = self.lock();
                        soft_delete_by_remote_id(&conn, remote_id)?;
                        result.deleted += 1;
                        result.details.push(format!(
                            "Conversation #{number} no longer exists remotely - marked deleted locally"
                        ));
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("merged into") {
                            // 301 merged: mark the local row merged into its
                            // target when the target exists locally (the
                            // reference parses the merge target from the
                            // error and marks the conversation).
                            let target = msg
                                .split_whitespace()
                                .position(|w| w == "into")
                                .and_then(|at| {
                                    msg.split_whitespace()
                                        .nth(at + 1)
                                        .and_then(|w| w.parse::<i64>().ok())
                                });
                            let marked = {
                                let conn = self.lock();
                                match target.and_then(|t| conversation_local_id(&conn, t)) {
                                    Some(target_local) => conn
                                        .execute(
                                            "UPDATE conversations SET merged_into_conversation_id = ?1,
                                                updated_at = datetime('now')
                                              WHERE id = ?2",
                                            params![target_local, local_id],
                                        )
                                        .is_ok(),
                                    None => false,
                                }
                            };
                            let _ = marked;
                            result.merged += 1;
                            result
                                .details
                                .push(format!("Conversation #{number} merged into another conversation"));
                        } else {
                            result.failed += 1;
                        }
                    }
                }
            }

            // Phase 3: orphaned FTS rows — conversations deleted locally but
            // threads still unindexed (detect index failures and heal).
            // No-op until the FTS layer exists (it ships with the search
            // engine module).
            let fts_exists: bool = {
                let conn = self.lock();
                conn.query_row(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'fts_threads'",
                    [],
                    |_| Ok(()),
                )
                .is_ok()
            };
            let orphans: i64 = if fts_exists {
                let conn = self.lock();
                conn.query_row(
                    "SELECT COUNT(*) FROM conversation_threads
                      WHERE (fts_indexed IS NULL OR fts_indexed = 0)
                        AND body IS NOT NULL AND LENGTH(body) > 0",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0)
            } else {
                0
            };
            if orphans > 0 {
                let conn = self.lock();
                conn.execute(
                    "INSERT INTO fts_threads (body, thread_id, conversation_id)
                     SELECT t.body, t.id, t.conversation_id FROM conversation_threads t
                      WHERE (t.fts_indexed IS NULL OR t.fts_indexed = 0)
                        AND t.body IS NOT NULL AND LENGTH(t.body) > 0
                        AND NOT EXISTS (SELECT 1 FROM fts_threads f WHERE f.thread_id = t.id)",
                    [],
                )?;
                conn.execute(
                    "UPDATE conversation_threads SET fts_indexed = 1
                      WHERE (fts_indexed IS NULL OR fts_indexed = 0)
                        AND body IS NOT NULL AND LENGTH(body) > 0",
                    [],
                )?;
                result
                    .details
                    .push(format!("Rebuilt FTS index for {orphans} threads"));
            }

            let conn = self.lock();
            set_state(&conn, "LIVE");
            let _ = update_run(
                &conn,
                run_id,
                RunUpdate {
                    state: Some("LIVE"),
                    resources_done: None,
                    resources_total: None,
                    records_processed: None,
                    errors: None,
                    detail: Some(serde_json::to_value(&result).unwrap_or(Value::Null)),
                    finished: true,
                },
            );
            Ok(())
        }
        .await;
        if let Err(e) = outcome {
            let conn = self.lock();
            set_state(&conn, "ERROR");
            let _ = update_run(
                &conn,
                run_id,
                RunUpdate {
                    state: Some("ERROR"),
                    resources_done: None,
                    resources_total: None,
                    records_processed: None,
                    errors: None,
                    detail: Some(json!({ "error": e.to_string() })),
                    finished: true,
                },
            );
            let _ = crate::jobs::log_error(&conn, "sync", &format!("Reconciliation failed: {e}"));
            result.failed += 1;
        }
        self.running.store(false, Ordering::SeqCst);
        Ok(result)
    }

    // =================================================================
    // Single-conversation sync (webhooks + manual refresh)
    // =================================================================
    pub async fn sync_single_conversation(&self, remote_id: i64) -> Result<bool> {
        let remote = self.provider.get_conversation(remote_id).await;
        match remote {
            Ok(Some(conv)) => {
                let threads = self
                    .provider
                    .list_threads(remote_id)
                    .await
                    .unwrap_or_default();
                self.ingest_conversation(&conv, &threads, false).await?;
                Ok(true)
            }
            Ok(None) => {
                let conn = self.lock();
                soft_delete_by_remote_id(&conn, remote_id)?;
                Ok(true)
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("merged into") || msg.contains("-> 404") {
                    let conn = self.lock();
                    soft_delete_by_remote_id(&conn, remote_id)?;
                    Ok(true)
                } else {
                    Err(e)
                }
            }
        }
    }

    // =================================================================
    // Resource sync implementations (the reference switch)
    // =================================================================
    /// Public wrapper so the worker job executor can run single-resource
    /// syncs (`sync_tags`, `sync_user_statuses`, ...) through the same
    /// reference switch the coordinator uses.
    pub async fn run_resource(
        &self,
        resource: &str,
        initial: bool,
    ) -> Result<ResourceSyncResult> {
        self.sync_resource(resource, initial).await
    }

    async fn sync_resource(&self, resource: &str, initial: bool) -> Result<ResourceSyncResult> {
        let resource = resource.to_string();
        match resource.as_str() {
            "account" => {
                let me = self.provider.get_me().await?;
                let conn = self.lock();
                conn.execute(
                    "INSERT INTO accounts (remote_id, plan, raw_json, last_seen_at)
                     VALUES (?1, ?2, ?3, datetime('now'))
                     ON CONFLICT(remote_id) DO UPDATE SET plan = excluded.plan,
                       raw_json = excluded.raw_json, last_seen_at = datetime('now')",
                    params![
                        me.remote_id,
                        me.role,
                        serde_json::to_string(&me).unwrap_or_default()
                    ],
                )?;
                mirror::upsert_user(&conn, &me)?;
                Ok(ResourceSyncResult {
                    resource,
                    processed: 1,
                    failed: 0,
                    error: None,
                })
            }
            "users" => {
                let users = self.provider.list_users().await?;
                let conn = self.lock();
                for u in &users {
                    mirror::upsert_user(&conn, u)?;
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed: users.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "system_users" => {
                let users = self.provider.list_system_users().await?;
                let conn = self.lock();
                for u in &users {
                    mirror::upsert_user(&conn, u)?;
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed: users.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "teams" => {
                let teams = self.provider.list_teams().await?;
                let conn = self.lock();
                for t in &teams {
                    mirror::upsert_team(&conn, t)?;
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed: teams.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "mailboxes" => {
                let mbs = self.provider.list_mailboxes().await?;
                let conn = self.lock();
                for m in &mbs {
                    mirror::upsert_mailbox(&conn, m)?;
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed: mbs.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "folders" => {
                let mbs = self.provider.list_mailboxes().await?;
                let mut processed = 0;
                for m in &mbs {
                    let folders = self.provider.list_folders(m.remote_id).await?;
                    let conn = self.lock();
                    if let Some(mailbox_local) = local_id(&conn, "mailboxes", m.remote_id) {
                        for f in &folders {
                            upsert_folder(&conn, mailbox_local, f)?;
                            processed += 1;
                        }
                    }
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed,
                    failed: 0,
                    error: None,
                })
            }
            "tags" => {
                let tags = self.provider.list_tags().await?;
                let conn = self.lock();
                for t in &tags {
                    mirror::upsert_tag(&conn, t)?;
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed: tags.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "inbox_fields" => {
                let mbs = self.provider.list_mailboxes().await?;
                let mut processed = 0;
                for m in &mbs {
                    let fields = self.provider.list_inbox_fields(m.remote_id).await?;
                    let conn = self.lock();
                    if let Some(mailbox_local) = local_id(&conn, "mailboxes", m.remote_id) {
                        for f in &fields {
                            upsert_inbox_field(&conn, mailbox_local, f)?;
                            processed += 1;
                        }
                    }
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed,
                    failed: 0,
                    error: None,
                })
            }
            "customer_property_definitions" => {
                let defs = self.provider.list_customer_property_definitions().await?;
                let conn = self.lock();
                upsert_property_definitions(&conn, "customer_property_definitions", &defs)?;
                Ok(ResourceSyncResult {
                    resource,
                    processed: defs.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "organization_property_definitions" => {
                let defs = self
                    .provider
                    .list_organization_property_definitions()
                    .await?;
                let conn = self.lock();
                upsert_property_definitions(&conn, "organization_property_definitions", &defs)?;
                Ok(ResourceSyncResult {
                    resource,
                    processed: defs.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "organizations" => {
                let orgs = self.provider.list_organizations().await?;
                let conn = self.lock();
                for o in &orgs {
                    upsert_organization(&conn, o)?;
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed: orgs.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "customers" => {
                let since = if initial {
                    None
                } else {
                    let conn = self.lock();
                    get_incremental_since(&conn, "customers", SYNC_OVERLAP_MINUTES)
                };
                self.sync_customers(since.as_deref()).await
            }
            "saved_replies" => {
                let mbs = self.provider.list_mailboxes().await?;
                let mut processed = 0;
                for m in &mbs {
                    let replies = self.provider.list_saved_replies(m.remote_id).await?;
                    let conn = self.lock();
                    let mailbox_local = local_id(&conn, "mailboxes", m.remote_id);
                    for r in &replies {
                        upsert_saved_reply(&conn, mailbox_local, r)?;
                        processed += 1;
                    }
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed,
                    failed: 0,
                    error: None,
                })
            }
            "workflows" => {
                let wfs = self.provider.list_workflows().await?;
                let conn = self.lock();
                for w in &wfs {
                    upsert_workflow(&conn, w)?;
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed: wfs.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "conversations" | "threads" => self.sync_conversations(None, initial).await,
            "chats" => {
                // Chat sessions are type='chat' conversations; the main pass
                // mirrors them — this pass heals gaps and checkpoints itself.
                let chats = self.provider.list_beacon_chats().await?;
                let mut processed = 0;
                for c in &chats {
                    let existing = {
                        let conn = self.lock();
                        conversation_local_id(&conn, c.remote_id)
                    };
                    if existing.is_none() {
                        let conv = self.provider.get_conversation(c.remote_id).await?;
                        if let Some(conv) = conv {
                            self.ingest_conversation(&conv, &[], true).await?;
                            processed += 1;
                        }
                    }
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed,
                    failed: 0,
                    error: None,
                })
            }
            "attachments" => {
                // Metadata already stored with threads; the reference returns
                // the pending-download count. The port defers downloads to the
                // worker (documented intentional difference).
                Ok(ResourceSyncResult {
                    resource,
                    processed: 0,
                    failed: 0,
                    error: None,
                })
            }
            "ratings" => {
                let ratings = self.provider.list_ratings().await?;
                let mut fresh = 0;
                for r in &ratings {
                    let (inserted, conv_local, conv_number, customer_local) = {
                        let conn = self.lock();
                        let conv_local = r
                            .conversation_id
                            .and_then(|id| {
                                if id > 0 {
                                    conversation_local_id(&conn, id)
                                } else {
                                    None
                                }
                            });
                        let conv_number = conv_local.and_then(|local| {
                            conn.query_row(
                                "SELECT number FROM conversations WHERE id = ?1",
                                params![local],
                                |row| row.get::<_, i64>(0),
                            )
                            .ok()
                        });
                        let customer_local = r
                            .customer_id
                            .and_then(|id| {
                                if id > 0 {
                                    local_id(&conn, "customers", id)
                                } else {
                                    None
                                }
                            });
                        let user_local = r
                            .user_id
                            .and_then(|id| if id > 0 { local_id(&conn, "users", id) } else { None });
                        let n = conn.execute(
                            "INSERT INTO ratings (remote_id, conversation_id, rating, comments,
                                 customer_local_id, user_local_id, remote_created_at, last_synced_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
                             ON CONFLICT(remote_id) DO UPDATE SET rating = excluded.rating,
                               comments = excluded.comments, last_synced_at = datetime('now')",
                            params![
                                r.remote_id,
                                conv_local,
                                r.rating,
                                r.comment,
                                customer_local,
                                user_local,
                                r.created_at
                            ],
                        )?;
                        (n > 0, conv_local, conv_number, customer_local)
                    };
                    if inserted {
                        fresh += 1;
                        if let Some(bus) = &self.bus {
                            use crate::events::ServerEvent;
                            // The reference SSE payload carries the raw
                            // lowercase word ('great' | 'okay' | 'not-good').
                            bus.emit(&ServerEvent::rating_received(
                                r.rating.clone(),
                                conv_local,
                                conv_number,
                                customer_local,
                                r.customer_name.clone(),
                                r.comment.clone(),
                            ));
                        }
                    }
                }
                if fresh > 0 {
                    if let Some(bus) = &self.bus {
                        use crate::events::ServerEvent;
                        bus.emit(&ServerEvent::ratings_refreshed(
                            ratings.len() as u32,
                            fresh as u32,
                        ));
                    }
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed: ratings.len() as i64,
                    failed: 0,
                    error: None,
                })
            }
            "docs_collections" => {
                let collections = self.provider.list_doc_collections().await?;
                let mut processed = 0;
                for col in &collections {
                    let categories = self.provider.list_doc_categories(col.remote_id).await?;
                    let conn = self.lock();
                    let local = upsert_doc_collection(&conn, col)?;
                    upsert_doc_categories(&conn, local, &categories)?;
                    processed += 1;
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed,
                    failed: 0,
                    error: None,
                })
            }
            "docs_articles" => {
                let mut processed = 0;
                let collections: Vec<i64> = {
                    let conn = self.lock();
                    let mut stmt = conn.prepare("SELECT remote_id FROM docs_collections")?;
                    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
                    rows.filter_map(|r| r.ok()).collect()
                };
                for col_remote in collections {
                    let articles = self.provider.list_doc_articles(col_remote).await?;
                    let conn = self.lock();
                    if let Some(col_local) = local_id(&conn, "docs_collections", col_remote) {
                        for a in &articles {
                            upsert_doc_article(&conn, a, col_local)?;
                            processed += 1;
                        }
                    }
                }
                // Semantic docs search: enqueue embedding rebuilds (no-op until
                // an embedding model is configured) — reference parity.
                if processed > 0 {
                    let conn = self.lock();
                    let _ =
                        crate::jobs::enqueue_on(&conn, "embeddings", "embed_docs_chunks", "{}", 2);
                    let _ = crate::jobs::enqueue_on(
                        &conn,
                        "embeddings",
                        "embed_conversation_chunks",
                        "{}",
                        2,
                    );
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed,
                    failed: 0,
                    error: None,
                })
            }
            "user_statuses" => {
                let users: Vec<(i64, i64)> = {
                    let conn = self.lock();
                    let mut stmt = conn.prepare("SELECT id, remote_id FROM users")?;
                    let rows =
                        stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
                    rows.filter_map(|r| r.ok()).collect()
                };
                let mut processed = 0;
                for (local, remote) in users {
                    if let Some(status) = self.provider.get_user_status(remote).await? {
                        let conn = self.lock();
                        upsert_user_status(&conn, local, &status)?;
                        processed += 1;
                    }
                }
                Ok(ResourceSyncResult {
                    resource,
                    processed,
                    failed: 0,
                    error: None,
                })
            }
            "report_data" => Ok(ResourceSyncResult {
                resource,
                processed: 0,
                failed: 0,
                error: None,
            }),
            other => Ok(ResourceSyncResult {
                resource: other.to_string(),
                processed: 0,
                failed: 0,
                error: Some(format!("Unknown resource {other}")),
            }),
        }
    }

    async fn sync_customers(&self, since: Option<&str>) -> Result<ResourceSyncResult> {
        let resource = "customers".to_string();
        let mut processed = 0;
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .provider
                .list_customers(&CustomerQuery {
                    modified_since: since.map(|s| s.to_string()),
                    cursor: cursor.clone(),
                    ..Default::default()
                })
                .await?;
            {
                let conn = self.lock();
                for c in &page.items {
                    mirror::upsert_customer(&conn, c)?;
                    processed += 1;
                }
            }
            let empty = page.items.is_empty();
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
            if since.is_some() && empty {
                break;
            }
        }
        Ok(ResourceSyncResult {
            resource,
            processed,
            failed: 0,
            error: None,
        })
    }

    async fn sync_conversations(
        &self,
        since: Option<&str>,
        initial: bool,
    ) -> Result<ResourceSyncResult> {
        let resource = "conversations".to_string();
        let mut processed = 0;
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .provider
                .list_conversations(&ConversationQuery {
                    status: Some("all".into()),
                    modified_since: since.map(|s| s.to_string()),
                    cursor: cursor.clone(),
                    ..Default::default()
                })
                .await?;
            for c in &page.items {
                let threads = if initial {
                    // Initial pass fetches full threads per conversation
                    // (reference uses embed=threads; the port fetches).
                    self.provider
                        .list_threads(c.remote_id)
                        .await
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                self.ingest_conversation(c, &threads, !initial).await?;
                processed += 1;
            }
            let empty = page.items.is_empty();
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
            if !initial && empty {
                break;
            }
        }
        Ok(ResourceSyncResult {
            resource,
            processed,
            failed: 0,
            error: None,
        })
    }

    /// Persist one conversation (+threads) locally.
    async fn ingest_conversation(
        &self,
        c: &HsConversation,
        threads: &[HsThread],
        fetch_threads_if_needed: bool,
    ) -> Result<()> {
        // Ensure mailbox reference exists.
        let need_mailboxes = {
            let conn = self.lock();
            local_id(&conn, "mailboxes", c.mailbox_id).is_none()
        };
        if need_mailboxes {
            let mbs = self.provider.list_mailboxes().await?;
            let conn = self.lock();
            for m in &mbs {
                if local_id(&conn, "mailboxes", m.remote_id).is_none() {
                    mirror::upsert_mailbox(&conn, m)?;
                }
            }
        }
        // Ensure primary customer exists locally.
        let need_customer = {
            let conn = self.lock();
            c.customer_id > 0 && local_id(&conn, "customers", c.customer_id).is_none()
        };
        if need_customer {
            if let Some(cust) = self.provider.get_customer(c.customer_id).await? {
                let conn = self.lock();
                mirror::upsert_customer(&conn, &cust)?;
            }
        }
        {
            let conn = self.lock();
            mirror::upsert_conversation(&conn, c)?;
        }
        let local = {
            let conn = self.lock();
            conversation_local_id(&conn, c.remote_id)
        };
        let Some(local_id) = local else {
            return Ok(());
        };

        let mut list: Vec<HsThread> = threads.to_vec();
        if fetch_threads_if_needed || list.is_empty() {
            match self.provider.list_threads(c.remote_id).await {
                Ok(fetched) => list = fetched,
                Err(e) => {
                    // Threads fetch can fail for locked conversations —
                    // conversation data is still preserved.
                    let conn = self.lock();
                    let _ = crate::jobs::log_error(
                        &conn,
                        "sync",
                        &format!("Thread fetch failed for conversation {}: {e}", c.remote_id),
                    );
                    list = Vec::new();
                }
            }
        }
        let seen: Vec<i64> = list.iter().map(|t| t.remote_id).collect();
        {
            let conn = self.lock();
            for t in &list {
                upsert_thread(&conn, local_id, t)?;
            }
            // Remove local threads that no longer exist remotely.
            if !list.is_empty() || !threads.is_empty() {
                let mut stmt =
                    conn.prepare("SELECT id, remote_id FROM conversation_threads WHERE conversation_id = ?1 AND remote_id IS NOT NULL")?;
                let stale: Vec<i64> = stmt
                    .query_map(params![local_id], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                    })?
                    .filter_map(|r| r.ok())
                    .filter(|(_, remote)| !seen.contains(remote))
                    .map(|(id, _)| id)
                    .collect();
                for id in stale {
                    let _ = conn.execute(
                        "DELETE FROM conversation_threads WHERE id = ?1",
                        params![id],
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpscout::FakeHelpScoutProvider;
    use tempfile::NamedTempFile;

    fn fresh_conn() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        crate::inbox::apply_m028(&conn).unwrap();
        crate::sync_schema::apply_m029(&conn).unwrap();
        crate::customer_events::apply_m036(&conn).unwrap();
        crate::jobs::ensure_jobs_table(&conn).unwrap();
        conn
    }

    fn engine() -> (Arc<Mutex<Connection>>, SyncEngine) {
        let conn = Arc::new(Mutex::new(fresh_conn()));
        let engine = SyncEngine::new(conn.clone(), Arc::new(FakeHelpScoutProvider::new_demo()));
        (conn, engine)
    }

    #[tokio::test]
    async fn initial_sync_mirrors_the_demo_world() {
        let (conn, engine) = engine();
        let results = engine.initial_sync().await.unwrap();
        assert_eq!(results.len(), INITIAL_SYNC_ORDER.len());
        assert!(!engine.is_running());
        let conn = conn.lock().unwrap();
        // State transitions to LIVE when nothing failed.
        assert_eq!(get_state(&conn), "LIVE");
        // Reference data mirrored.
        let (mailboxes, users, tags, orgs, folders, fields): (i64, i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM mailboxes), (SELECT COUNT(*) FROM users),
                        (SELECT COUNT(*) FROM tags), (SELECT COUNT(*) FROM organizations),
                        (SELECT COUNT(*) FROM folders), (SELECT COUNT(*) FROM inbox_fields)",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(mailboxes, 2);
        assert_eq!(users, 3);
        assert_eq!(tags, 14);
        assert_eq!(orgs, 2);
        assert_eq!(folders, 4);
        assert!(fields >= 3);
        // Conversations + threads mirrored.
        let (convs, threads): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM conversations), (SELECT COUNT(*) FROM conversation_threads)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(convs, 10);
        assert_eq!(threads, 20);
        // Saved replies / workflows / docs mirrored.
        let (replies, workflows, docs): (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM saved_replies), (SELECT COUNT(*) FROM workflows),
                        (SELECT COUNT(*) FROM docs)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(replies, 3);
        assert_eq!(workflows, 3);
        assert_eq!(docs, 3);
        // Checkpoints recorded for every resource with ok status.
        let ok: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_checkpoints WHERE status = 'ok'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(ok >= 20, "expected >= 20 ok checkpoints, got {ok}");
    }

    #[tokio::test]
    async fn sync_runs_recorded_with_reference_shape() {
        let (conn, engine) = engine();
        engine.initial_sync().await.unwrap();
        let conn = conn.lock().unwrap();
        let (kind, state, total, finished): (String, String, i64, Option<String>) = conn
            .query_row(
                "SELECT kind, state, resources_total, finished_at FROM sync_runs ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "initial");
        assert_eq!(state, "LIVE");
        assert_eq!(total, INITIAL_SYNC_ORDER.len() as i64);
        assert!(finished.is_some());
    }

    #[tokio::test]
    async fn incremental_sync_uses_overlap_watermark() {
        let (conn, engine) = engine();
        engine.initial_sync().await.unwrap();
        let results = engine.incremental_sync().await.unwrap();
        assert!(!results.is_empty());
        let conn = conn.lock().unwrap();
        assert_eq!(get_state(&conn), "LIVE");
    }

    #[tokio::test]
    async fn reconcile_passes_over_the_mirror() {
        let (conn, engine) = engine();
        engine.initial_sync().await.unwrap();
        let result = engine.reconcile().await.unwrap();
        assert_eq!(result.added, 0);
        assert_eq!(result.failed, 0);
        assert!(result.checked >= 10);
        let conn = conn.lock().unwrap();
        assert_eq!(get_state(&conn), "LIVE");
    }

    #[tokio::test]
    async fn running_sync_rejects_concurrent_start() {
        let (conn, engine) = engine();
        // Simulate a running sync.
        engine.running.store(true, Ordering::SeqCst);
        let err = engine.initial_sync().await.unwrap_err();
        assert!(err.to_string().contains("already running"));
        engine.running.store(false, Ordering::SeqCst);
        drop(conn);
    }

    #[tokio::test]
    async fn single_conversation_sync_refreshes() {
        let (conn, engine) = engine();
        engine.initial_sync().await.unwrap();
        let ok = engine.sync_single_conversation(1001).await.unwrap();
        assert!(ok);
        let conn = conn.lock().unwrap();
        let threads: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_threads WHERE conversation_id =
                   (SELECT id FROM conversations WHERE remote_id = 1001)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(threads, 2);
    }

    #[tokio::test]
    async fn single_conversation_sync_deletes_missing() {
        let (conn, engine) = engine();
        engine.initial_sync().await.unwrap();
        let ok = engine.sync_single_conversation(999_999).await.unwrap();
        assert!(ok);
        let conn = conn.lock().unwrap();
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM conversations WHERE remote_id = 999999",
                [],
                |r| r.get(0),
            )
            .ok();
        assert!(status.is_none());
    }

    #[test]
    fn incremental_since_computes_overlap() {
        let conn = fresh_conn();
        record_success(&conn, "conversations", 5);
        let since = get_incremental_since(&conn, "conversations", SYNC_OVERLAP_MINUTES);
        assert!(since.is_some());
        // Fresh resource has no watermark.
        assert!(get_incremental_since(&conn, "unknown", 10).is_none());
    }

    #[test]
    fn state_round_trips_through_application_settings() {
        let conn = fresh_conn();
        assert_eq!(get_state(&conn), "NEW");
        set_state(&conn, "CATCHING_UP");
        assert_eq!(get_state(&conn), "CATCHING_UP");
    }
}
