//! WorkerManager (spec #110, reference `services/workers.ts`): explicit
//! queues for sync, api, attachments, embeddings, AI analysis/drafting/
//! verification, reports, maintenance, reconciliation. Background work
//! NEVER blocks request handling; jobs are claimed atomically from SQLite
//! and survive restarts.
//!
//! Eight timers mirror the reference exactly:
//! 1. job loop — every 2s (bounded: ≤10 claims per tick)
//! 2. incremental sync — `sync_interval_minutes` (default 5, clamp 1..1440)
//! 3. ratings refresh — `ratings_refresh_seconds` (default 30, clamp 0..3600;
//!    0 disables)
//! 4. maintenance — every 6h (trends, products, sweeps, backup, retention)
//! 5. stale-job sweep — every 10 min (30-minute rule)
//! 6. notification sweep — `notification_sweep_seconds` (default 15, clamp
//!    5..3600) + boot catch-up
//! 7. customer-event sweep — `customer_event_sweep_seconds` (default 60,
//!    clamp 15..3600) + boot catch-up
//! 8. connector refresh tick — every 30s

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::Result;
use crate::helpscout::HelpScoutProvider;
use crate::http::EventBus;
use crate::jobs;
use crate::sync_engine::SyncEngine;

/// Default for `ratings_refresh_seconds` (reference constants.ts).
pub const RATINGS_REFRESH_DEFAULT_SECONDS: i64 = 30;

pub struct WorkerManager {
    conn: Arc<Mutex<Connection>>,
    engine: Option<Arc<SyncEngine>>,
    provider: Arc<dyn HelpScoutProvider>,
    bus: EventBus,
    data_dir: PathBuf,
    /// The embedded Qdrant adapter (reference `ctx.qdrant`, deviation D2) —
    /// the same `Arc` the AppState holds, so settings reconfigures reach
    /// the workers. `None` in unit tests.
    qdrant: Option<Arc<crate::vectorstore_qdrant::EmbeddedQdrant>>,
    running: AtomicBool,
    stopped: AtomicBool,
    processing: AtomicBool,
    refreshing_ratings: AtomicBool,
    sweeping_notifications: AtomicBool,
    sweeping_customer_events: AtomicBool,
    refreshing_connectors: AtomicBool,
    handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl WorkerManager {
    #[must_use]
    pub fn new(
        conn: Arc<Mutex<Connection>>,
        engine: Option<Arc<SyncEngine>>,
        provider: Arc<dyn HelpScoutProvider>,
        bus: EventBus,
        data_dir: PathBuf,
        qdrant: Option<Arc<crate::vectorstore_qdrant::EmbeddedQdrant>>,
    ) -> Self {
        Self {
            conn,
            engine,
            provider,
            bus,
            data_dir,
            qdrant,
            running: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            refreshing_ratings: AtomicBool::new(false),
            sweeping_notifications: AtomicBool::new(false),
            sweeping_customer_events: AtomicBool::new(false),
            refreshing_connectors: AtomicBool::new(false),
            handles: Mutex::new(Vec::new()),
        }
    }

    /// Whether the worker loop is running (`/api/system/status`).
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(task);
        self.handles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(handle);
    }

    /// Start every timer. Idempotent (a second call is a no-op).
    /// Callers hold the manager inside an `Arc` (the server boot path
    /// wraps it once and stores it on the AppState), so every timer task
    /// keeps the manager alive for its whole lifetime.
    pub fn start(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stopped.store(false, Ordering::SeqCst);
        // Recover stale jobs from a previous run (spec: survive restart).
        {
            let conn = self.lock();
            match jobs::recover_stale_jobs(&conn) {
                Ok(n) if n > 0 => {
                    tracing::info!(
                        count = n,
                        operation = "recover",
                        "Recovered stale jobs after restart"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "stale-job recovery failed"),
            }
            // Recover webhook events persisted but never processed.
            let drained = crate::webhook_handler::drain_pending(&conn);
            if drained > 0 {
                tracing::info!(
                    count = drained,
                    operation = "webhook_drain",
                    "Drained pending webhook events after restart"
                );
            }
        }

        // 1. Main job loop every 2s.
        {
            let this = self.clone();
            self.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(2));
                loop {
                    interval.tick().await;
                    this.tick().await;
                }
            });
        }
        // 2. Periodic incremental sync (guarded against a malformed stored
        //    interval: a non-finite value previously collapsed to a runaway).
        {
            let (minutes, raw) = {
                let conn = self.lock();
                let raw = crate::settings::get_i64(&conn, "sync_interval_minutes", 5).unwrap_or(5);
                let clamped = raw.clamp(1, 1440);
                (clamped, raw)
            };
            let this = self.clone();
            self.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(
                    u64::try_from(minutes * 60).unwrap_or(300),
                ));
                loop {
                    interval.tick().await;
                    this.auto_sync().await;
                }
            });
            tracing::info!(
                operation = "start",
                sync_interval_minutes = raw,
                "worker auto-sync timer armed"
            );
        }
        // 3. Real-time ratings refresh (0 disables).
        {
            let seconds = {
                let conn = self.lock();
                crate::settings::get_i64(
                    &conn,
                    "ratings_refresh_seconds",
                    RATINGS_REFRESH_DEFAULT_SECONDS,
                )
                .unwrap_or(RATINGS_REFRESH_DEFAULT_SECONDS)
                .clamp(0, 3600)
            };
            if seconds > 0 {
                let this = self.clone();
                self.spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(
                        u64::try_from(seconds).unwrap_or(30),
                    ));
                    loop {
                        interval.tick().await;
                        this.refresh_ratings().await;
                    }
                });
            }
        }
        // 4. Maintenance: backup + cluster trends + cleanup every 6h.
        {
            let this = self.clone();
            self.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(6 * 3600));
                loop {
                    interval.tick().await;
                    this.maintenance().await;
                }
            });
        }
        // 5. Stale-job sweep (v2.2.1 audit fix): the boot recovery alone left
        //    a job claimed right before restart stuck 'running' forever.
        {
            let this = self.clone();
            self.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(10 * 60));
                loop {
                    interval.tick().await;
                    let conn = this.lock();
                    match jobs::recover_stale_jobs(&conn) {
                        Ok(n) if n > 0 => tracing::warn!(
                            count = n,
                            operation = "stale_job_sweep",
                            "Re-queued stale running job(s) (running longer than 30 minutes)"
                        ),
                        Ok(_) => {}
                        Err(_) => {}
                    }
                }
            });
        }
        // 6. Notification sweep (the single producer of Notification Center
        //    rows) + catch-up at boot (a fresh DB initializes its cursor
        //    silently).
        {
            let seconds = {
                let conn = self.lock();
                crate::settings::get_i64(&conn, "notification_sweep_seconds", 15)
                    .unwrap_or(15)
                    .clamp(5, 3600)
            };
            self.notification_sweep_tick();
            let this = self.clone();
            self.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(
                    u64::try_from(seconds).unwrap_or(15),
                ));
                loop {
                    interval.tick().await;
                    this.notification_sweep_tick();
                }
            });
        }
        // 7. Customer event timeline sweep + boot catch-up.
        {
            let seconds = {
                let conn = self.lock();
                crate::settings::get_i64(&conn, "customer_event_sweep_seconds", 60)
                    .unwrap_or(60)
                    .clamp(15, 3600)
            };
            self.customer_event_sweep_tick();
            let this = self.clone();
            self.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(
                    u64::try_from(seconds).unwrap_or(60),
                ));
                loop {
                    interval.tick().await;
                    this.customer_event_sweep_tick();
                }
            });
        }
        // 8. Connector interval refresh: a shared tick checks every 30s and
        //    refreshes whatever is due. Failures land in health.
        {
            let this = self.clone();
            self.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    this.connector_refresh_tick().await;
                }
            });
        }
        tracing::info!(operation = "start", "Background workers started");
    }

    /// Stop every timer. In-flight jobs keep their existing crash-recovery
    /// semantics ("jobs survive restarts").
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        let mut handles = self.handles.lock().unwrap_or_else(|p| p.into_inner());
        for h in handles.drain(..) {
            h.abort();
        }
        tracing::info!(operation = "stop", "Background workers stopped");
    }

    // ─── Timer bodies ──────────────────────────────────────────────────────

    /// One bounded pass over the job queues (≤10 claims; never blocks on
    /// stuck jobs).
    pub async fn tick(&self) {
        if self.stopped.load(Ordering::SeqCst) || self.processing.swap(true, Ordering::SeqCst) {
            return;
        }
        for _ in 0..10 {
            let claimed = {
                let mut conn = self.lock();
                jobs::claim_next(&mut conn).unwrap_or(None)
            };
            let Some(job) = claimed else { break };
            self.execute_job(job.id, &job.kind, &job.payload).await;
        }
        self.processing.store(false, Ordering::SeqCst);
    }

    /// Skip while the initial sync is still populating the mirror.
    async fn auto_sync(&self) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let state = {
            let conn = self.lock();
            crate::sync_engine::get_state(&conn)
        };
        if state == "NEW" || state == "INITIALIZING" || state == "BACKFILLING" {
            return;
        }
        let Some(engine) = self.engine.clone() else {
            return;
        };
        if let Err(e) = engine.incremental_sync().await {
            tracing::warn!(error = %e, operation = "incremental_sync", "Incremental sync failed");
        }
    }

    /// Ratings-only refresh: fetch, upsert, emit `rating-received` for every
    /// NEW rating so SSE subscribers update within seconds. Failures are
    /// logged and never retried within the tick.
    async fn refresh_ratings(&self) {
        if self.stopped.load(Ordering::SeqCst)
            || self.refreshing_ratings.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let result: Result<()> = async {
            let ratings = self.provider.list_ratings().await?;
            let mut fresh = 0usize;
            for r in &ratings {
                let (inserted, conv_local, conv_number, customer_local) = {
                    let conn = self.lock();
                    let conv_local = r.conversation_id.and_then(|id| {
                        if id > 0 {
                            crate::sync_engine::conversation_local_id(&conn, id)
                        } else {
                            None
                        }
                    });
                    let conv_number = conv_local.and_then(|local| {
                        conn.query_row(
                            "SELECT number FROM conversations WHERE id = ?1",
                            rusqlite::params![local],
                            |row| row.get::<_, i64>(0),
                        )
                        .ok()
                    });
                    let customer_local = r.customer_id.and_then(|id| {
                        if id > 0 {
                            crate::sync_engine::local_id(&conn, "customers", id)
                        } else {
                            None
                        }
                    });
                    let user_local = r.user_id.and_then(|id| {
                        if id > 0 {
                            crate::sync_engine::local_id(&conn, "users", id)
                        } else {
                            None
                        }
                    });
                    (
                        upsert_rating_row(&conn, r, conv_local, customer_local, user_local)?,
                        conv_local,
                        conv_number,
                        customer_local,
                    )
                };
                if inserted {
                    fresh += 1;
                    // The reference SSE payload carries the raw lowercase
                    // word ('great' | 'okay' | 'not-good').
                    crate::http::event_bus::notify_rating_received(
                        &self.bus,
                        r.rating.as_deref(),
                        conv_local,
                        conv_number,
                        customer_local,
                        r.customer_name.as_deref(),
                        r.comment.as_deref(),
                    );
                }
            }
            if fresh > 0 {
                crate::http::event_bus::notify_ratings_refreshed(
                    &self.bus,
                    ratings.len() as u32,
                    fresh as u32,
                );
                tracing::info!(
                    operation = "ratings_refresh",
                    fresh,
                    processed = ratings.len(),
                    "Ratings refresh pushed new ratings"
                );
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, operation = "ratings_refresh", "Ratings refresh failed");
        }
        self.refreshing_ratings.store(false, Ordering::SeqCst);
    }

    /// Maintenance: trends + products + sweeps + backup + retention.
    async fn maintenance(&self) {
        let conn = self.conn.clone();
        let data_dir = self.data_dir.clone();
        let bus = self.bus.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::maintenance::compute_trends(&conn)?;
            // v2.2.0: deterministic products registry refresh — never breaks
            // maintenance.
            let _ = crate::maintenance::refresh_products(&conn);
            // v1.8.0: notification sweep piggybacks so long-idle instances
            // still produce state notifications.
            let _ = crate::notification_sweep::sweep(&conn, Some(&bus));
            // v1.6.0 audit fix: actually honor backup_interval_hours and
            // prune to the newest 20.
            let backup_hours =
                crate::settings::get_i64(&conn, "backup_interval_hours", 24).unwrap_or(24);
            if backup_hours > 0 {
                let backups_dir = data_dir.join("backups");
                let backups = crate::backup_service::list_backups(&backups_dir);
                let newest = backups.first();
                let due = match newest {
                    None => true,
                    Some(b) => {
                        let created = b
                            .get("created_at")
                            .and_then(|v| v.as_str())
                            .and_then(parse_iso_to_unix);
                        match created {
                            Some(ts) => std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs() as i64 - ts >= backup_hours * 3600)
                                .unwrap_or(true),
                            None => true,
                        }
                    }
                };
                if due {
                    let result = crate::backup_service::backup(&conn, &backups_dir);
                    if result.ok {
                        tracing::info!(operation = "backup", "Automatic backup created");
                    }
                }
            }
            let backups_dir = data_dir.join("backups");
            let pruned = crate::backup_service::prune_backups(&backups_dir, 20);
            if pruned > 0 {
                tracing::info!(operation = "backup", pruned, "Pruned old backups");
            }
            let retention_days = crate::settings::get_i64(&conn, "retention_days", 0).unwrap_or(0);
            let removed = crate::maintenance::enforce_retention(&conn, retention_days)?;
            if removed > 0 {
                tracing::info!(operation = "retention", removed, "Retention pruning");
            }
            Ok(())
        })
        .await
        .map_err(|e| crate::error::Error::Config(format!("maintenance task: {e}")))
        .and_then(|r| r);
        if let Err(e) = result {
            tracing::warn!(error = %e, operation = "maintenance", "Maintenance failed");
        }
    }

    /// One notification sweep pass (re-entrancy-guarded, never throws out).
    pub fn notification_sweep_tick(&self) {
        if self.stopped.load(Ordering::SeqCst)
            || self.sweeping_notifications.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let conn = self.lock();
        match crate::notification_sweep::sweep(&conn, Some(&self.bus)) {
            Ok(result) if result.created > 0 => {
                tracing::debug!(
                    operation = "notification_sweep",
                    created = result.created,
                    "Notification sweep created notifications"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, operation = "notification_sweep", "Notification sweep failed")
            }
        }
        self.sweeping_notifications.store(false, Ordering::SeqCst);
    }

    /// One customer-event sweep pass (guarded, never throws out).
    pub fn customer_event_sweep_tick(&self) {
        if self.stopped.load(Ordering::SeqCst)
            || self.sweeping_customer_events.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let conn = self.lock();
        match crate::customer_events::sweep(&conn) {
            Ok(created) if created > 0 => {
                tracing::debug!(
                    operation = "customer_event_sweep",
                    created,
                    "Customer event sweep derived events"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, operation = "customer_event_sweep", "Customer event sweep failed")
            }
        }
        self.sweeping_customer_events.store(false, Ordering::SeqCst);
    }

    /// Refresh every due interval connector (guarded; failures -> health).
    async fn connector_refresh_tick(&self) {
        if self.stopped.load(Ordering::SeqCst)
            || self.refreshing_connectors.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let due: Vec<i64> = {
            let conn = self.lock();
            crate::connectors::due_for_refresh(&conn).unwrap_or_default()
        };
        let data_dir = self.data_dir.clone();
        for id in due {
            match crate::connectors::refresh(&self.conn, id, &data_dir).await {
                Ok(result) if !result.ok => {
                    tracing::warn!(
                        operation = "connector_refresh",
                        connector = id,
                        error = result.error.as_deref().unwrap_or("unknown"),
                        "Connector refresh failed"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    operation = "connector_refresh",
                    connector = id,
                    error = %e,
                    "Connector refresh failed"
                ),
            }
        }
        self.refreshing_connectors.store(false, Ordering::SeqCst);
    }

    // ─── Job execution ─────────────────────────────────────────────────────

    /// Execute one claimed job by kind. Mirrors the reference switch:
    /// unknown kinds fail permanently (`Unknown job type`).
    async fn execute_job(&self, job_id: i64, kind: &str, payload: &str) {
        let started = std::time::Instant::now();
        let payload: serde_json::Value =
            serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
        let num = |key: &str| -> Option<i64> {
            payload.get(key).and_then(|v| v.as_i64()).or_else(|| {
                payload
                    .get(key)
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
            })
        };
        let outcome: Result<()> = async {
            match kind {
                // ---------- sync queue ----------
                "initial_sync" => {
                    self.run_engine(|e| async move {
                        e.initial_sync().await.map(|_| ())
                    })
                    .await?;
                    self.on_after_initial_sync();
                }
                "incremental_sync" => {
                    self.run_engine(|e| async move { e.incremental_sync().await.map(|_| ()) })
                        .await?;
                }
                "reconciliation" => {
                    self.run_engine(|e| async move { e.reconcile().await.map(|_| ()) })
                        .await?;
                }
                "sync_conversation" | "sync_conversation_merge" => {
                    if let Some(remote) = num("remoteId").or_else(|| num("conversationId")) {
                        self.run_engine(|e| async move { e.sync_single_conversation(remote).await.map(|_| ()) })
                            .await?;
                        if kind == "sync_conversation" {
                            self.emit_conversation_updated_for_remote(remote);
                        }
                    }
                }
                "delete_conversation" => {
                    if let Some(remote) = num("remoteId") {
                        let conn = self.lock();
                        conn.execute(
                            "UPDATE conversations SET status = 'deleted',
                                 deleted_at = COALESCE(deleted_at, datetime('now'))
                               WHERE remote_id = ?1",
                            rusqlite::params![remote],
                        )?;
                    }
                }
                "sync_customer" => {
                    if let Some(remote) = num("remoteId") {
                        if let Some(customer) = self.provider.get_customer(remote).await? {
                            let conn = self.lock();
                            crate::sync::upsert_customer(&conn, &customer)?;
                        }
                    }
                }
                "delete_customer" => {
                    if let Some(remote) = num("remoteId") {
                        let conn = self.lock();
                        conn.execute(
                            "UPDATE customers SET deleted_at = datetime('now') WHERE remote_id = ?1",
                            rusqlite::params![remote],
                        )?;
                    }
                }
                "sync_organizations" => {
                    let orgs = self.provider.list_organizations().await?;
                    let conn = self.lock();
                    for o in &orgs {
                        crate::sync_engine::upsert_organization(&conn, o)?;
                    }
                }
                "sync_tags" => {
                    let tags = self.provider.list_tags().await?;
                    let conn = self.lock();
                    for t in &tags {
                        crate::sync::upsert_tag(&conn, t)?;
                    }
                }
                "sync_user_statuses" => {
                    self.run_engine(|e| async move { e.run_resource("user_statuses", false).await.map(|_| ()) })
                        .await?;
                }
                "sync_conversation_ratings" => {
                    // v1.6.0 audit fix: the payload key is conversationId
                    // (the webhook producer); with a ratingId we fetch and
                    // store the actual rating, otherwise we fall back to
                    // re-syncing the conversation.
                    let rating_id = num("ratingId");
                    let conv_remote = num("conversationId").or_else(|| num("remoteId"));
                    if let Some(rating_id) = rating_id {
                        if let Ok(Some(r)) = self.provider.get_rating(rating_id).await {
                            let (inserted, conv_local, conv_number, customer_local) = {
                                let conn = self.lock();
                                let conv_local = r
                                    .conversation_id
                                    .and_then(|id| if id > 0 { crate::sync_engine::conversation_local_id(&conn, id) } else { None });
                                let conv_number = conv_local.and_then(|local| {
                                    conn.query_row(
                                        "SELECT number FROM conversations WHERE id = ?1",
                                        rusqlite::params![local],
                                        |row| row.get::<_, i64>(0),
                                    )
                                    .ok()
                                });
                                let customer_local = r
                                    .customer_id
                                    .and_then(|id| if id > 0 { crate::sync_engine::local_id(&conn, "customers", id) } else { None });
                                let user_local = r
                                    .user_id
                                    .and_then(|id| if id > 0 { crate::sync_engine::local_id(&conn, "users", id) } else { None });
                                (
                                    upsert_rating_row(&conn, &r, conv_local, customer_local, user_local)?,
                                    conv_local,
                                    conv_number,
                                    customer_local,
                                )
                            };
                            if inserted {
                                crate::http::event_bus::notify_rating_received(
                                    &self.bus,
                                    r.rating.as_deref(),
                                    conv_local,
                                    conv_number,
                                    customer_local,
                                    r.customer_name.as_deref(),
                                    r.comment.as_deref(),
                                );
                            }
                        }
                    }
                    if let Some(conv_remote) = conv_remote {
                        self.run_engine(|e| async move {
                            e.sync_single_conversation(conv_remote).await.map(|_| ())
                        })
                        .await?;
                    }
                }
                // ---------- embeddings queue ----------
                "embed_knowledge_chunks" => {
                    let conn = self.lock();
                    let n = crate::embeddings::embed_pending_knowledge(
                        &conn,
                        self.qdrant.as_deref(),
                    )
                    .unwrap_or(0);
                    tracing::debug!(operation = kind, embedded = n, "embedding pass");
                }
                "embed_docs_chunks" => {
                    let conn = self.lock();
                    let n = crate::embeddings::embed_pending_docs(
                        &conn,
                        self.qdrant.as_deref(),
                    )
                    .unwrap_or(0);
                    tracing::debug!(operation = kind, embedded = n, "embedding pass");
                }
                "embed_conversation_chunks" => {
                    let conn = self.lock();
                    let n = crate::embeddings::embed_pending_conversation_chunks(
                        &conn,
                        self.qdrant.as_deref(),
                    )
                    .unwrap_or(0);
                    tracing::debug!(operation = kind, embedded = n, "embedding pass");
                }
                // ---------- maintenance queue ----------
                "rebuild_search_index" => {
                    let conn = self.lock();
                    let n = crate::search::rebuild_indexes(&conn)?;
                    tracing::info!(operation = kind, conversations = n, "Search index rebuilt");
                }
                "rebuild_embeddings" => {
                    {
                        let conn = self.lock();
                        conn.execute(
                            "UPDATE knowledge_chunks SET embedding_state = 'not_indexed'",
                            [],
                        )?;
                        jobs::enqueue_on(&conn, "embeddings", "embed_knowledge_chunks", "{}", 2)?;
                    }
                }
                // ---------- outreach queue ----------
                // audit OR-02 / B3: campaign send executor. Picked off the
                // outreach queue and dispatched to outreach::send_batch
                // (batch 5, attempts 3, provider.createConversation,
                // sync-back, unknown-state reconcile).
                "outreach_send_batch" => {
                    if let Some(campaign_id) = num("campaignId") {
                        let summary = crate::outreach::send_batch(
                            &self.conn,
                            &self.provider,
                            campaign_id,
                        )
                        .await;
                        tracing::info!(
                            operation = kind,
                            campaign_id = campaign_id,
                            remaining = summary["remaining"].as_i64().unwrap_or(-1),
                            "Outreach send batch processed"
                        );
                    } else {
                        let conn = self.lock();
                        jobs::fail(&conn, job_id, "outreach_send_batch missing campaignId")?;
                        return Ok(());
                    }
                }
                _ => {
                    let conn = self.lock();
                    jobs::fail(&conn, job_id, &format!("Unknown job type: {kind}"))?;
                    return Ok(());
                }
            }
            let conn = self.lock();
            jobs::complete(&conn, job_id)?;
            tracing::debug!(
                operation = kind,
                latency_ms = started.elapsed().as_millis() as u64,
                "Job completed"
            );
            Ok(())
        }
        .await;
        if let Err(e) = outcome {
            let msg = e.to_string();
            let conn = self.lock();
            let retryable = true; // jobs::fail requeues when attempts remain
            if retryable {
                let _ = jobs::fail(&conn, job_id, &msg);
            }
            let _ = jobs::log_error(&conn, "workers", &format!("Job {kind} failed: {msg}"));
            tracing::warn!(operation = kind, error = %msg, "Job failed");
        }
    }

    /// After initial sync: enqueue attachment downloads + first embedding
    /// passes + interaction baselines (all optional, never break sync).
    fn on_after_initial_sync(&self) {
        let conn = self.lock();
        // v2.2.0: products registry over the fresh mirror.
        let _ = crate::maintenance::refresh_products(&conn);
        let auto_download =
            crate::settings::get_i64(&conn, "attachment_auto_download", 1).unwrap_or(1) != 0;
        let _ = jobs::enqueue_on(&conn, "embeddings", "embed_knowledge_chunks", "{}", 2);
        // v1.5.0: semantic ticket search over the fresh mirror.
        let _ = jobs::enqueue_on(&conn, "embeddings", "embed_conversation_chunks", "{}", 2);
        if auto_download {
            let _ = jobs::enqueue_on(&conn, "attachments", "download_recent_attachments", "{}", 2);
        }
    }

    /// Emit `conversation-updated` for a remote id (write-behind convergence).
    fn emit_conversation_updated_for_remote(&self, remote_id: i64) {
        use crate::events::ServerEvent;
        let conn = self.lock();
        let row: Option<(i64, i64, Option<i64>, Option<String>)> = conn
            .query_row(
                "SELECT id, number, mailbox_id, subject FROM conversations WHERE remote_id = ?1",
                rusqlite::params![remote_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        if let Some((id, number, mailbox_id, subject)) = row {
            self.bus.emit(&ServerEvent::conversation_updated(
                Some(id),
                Some(number),
                mailbox_id,
                subject,
                "sync",
            ));
        }
    }

    async fn run_engine<F, Fut>(&self, f: F) -> Result<()>
    where
        F: FnOnce(Arc<SyncEngine>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let Some(engine) = self.engine.clone() else {
            return Ok(());
        };
        f(engine).await
    }
}

#[cfg(test)]
fn iso_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    // Days since epoch -> civil date (Howard Hinnant's algorithm).
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

fn parse_iso_to_unix(s: &str) -> Option<i64> {
    // Handles both 'T'- and ' '-separated timestamps (SQLite datetime and ISO).
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    // Civil date -> days since epoch.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
fn rating_word(rating: &str) -> String {
    match rating {
        "great" | "5" | "Great" => "great".to_string(),
        "okay" | "3" | "Okay" => "okay".to_string(),
        _ => "not-good".to_string(),
    }
}

/// Upsert a rating row; returns whether this was a NEW rating.
fn upsert_rating_row(
    conn: &Connection,
    r: &crate::helpscout::HsRating,
    conversation_local_id: Option<i64>,
    customer_local_id: Option<i64>,
    user_local_id: Option<i64>,
) -> Result<bool> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM ratings WHERE remote_id = ?1",
            rusqlite::params![r.remote_id],
            |row| row.get(0),
        )
        .ok();
    conn.execute(
        "INSERT INTO ratings (remote_id, conversation_id, rating, comments,
             customer_local_id, user_local_id, remote_created_at, last_synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
         ON CONFLICT(remote_id) DO UPDATE SET
            rating = excluded.rating,
            comments = excluded.comments,
            last_synced_at = datetime('now')",
        rusqlite::params![
            r.remote_id,
            conversation_local_id,
            r.rating,
            r.comment,
            customer_local_id,
            user_local_id,
            r.created_at,
        ],
    )?;
    Ok(existing.is_none())
}

// ─── Boot helper ──────────────────────────────────────────────────────────

/// Build the manager and start every timer on the current tokio runtime.
/// Returns the shared handle the server stores for `is_running` probes and
/// graceful shutdown.
pub fn start_workers(
    conn: Arc<Mutex<Connection>>,
    engine: Option<Arc<SyncEngine>>,
    provider: Arc<dyn HelpScoutProvider>,
    bus: EventBus,
    data_dir: PathBuf,
    qdrant: Option<Arc<crate::vectorstore_qdrant::EmbeddedQdrant>>,
) -> Arc<WorkerManager> {
    let manager = Arc::new(WorkerManager::new(
        conn, engine, provider, bus, data_dir, qdrant,
    ));
    manager.start();
    manager
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_now_is_parseable_round_trip() {
        let now = iso_now();
        assert!(parse_iso_to_unix(&now).is_some());
        assert!(now.ends_with('Z'));
    }

    #[test]
    fn parse_iso_handles_both_separators() {
        let a = parse_iso_to_unix("2026-01-02T03:04:05Z").unwrap();
        let b = parse_iso_to_unix("2026-01-02 03:04:05").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, 1_767_323_045);
    }

    #[test]
    fn rating_words_match_reference() {
        assert_eq!(rating_word("great"), "great");
        assert_eq!(rating_word("okay"), "okay");
        assert_eq!(rating_word("not-good"), "not-good");
    }
}
