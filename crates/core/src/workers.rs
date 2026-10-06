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

use futures_util::FutureExt;
use rusqlite::Connection;
use std::panic::AssertUnwindSafe;

use crate::error::{Error, Result};
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

    /// Run one tick of a timer body with panic containment (C3, audit
    /// T16): a panicking tick is logged and skipped, and the timer keeps
    /// running. Previously, with the release profile on `panic = "abort"`, an
    /// unrecovered panic in ANY timer body killed the whole packaged app.
    async fn contained<F: std::future::Future>(label: &'static str, f: F) {
        if AssertUnwindSafe(f).catch_unwind().await.is_err() {
            tracing::error!(
                timer = label,
                "timer tick panicked (contained at the task boundary; timer continues)"
            );
        }
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
                    Self::contained("job_loop", this.tick()).await;
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
                    Self::contained("auto_sync", this.auto_sync()).await;
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
                        Self::contained("refresh_ratings", this.refresh_ratings()).await;
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
                    Self::contained("maintenance", this.maintenance()).await;
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
                    Self::contained("stale_job_sweep", async {
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
                    })
                    .await;
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
                    Self::contained("notification_sweep", async {
                        this.notification_sweep_tick();
                    })
                    .await;
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
                    Self::contained("customer_event_sweep", async {
                        this.customer_event_sweep_tick();
                    })
                    .await;
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
                    Self::contained("connector_refresh", this.connector_refresh_tick()).await;
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
            // C3 (audit T16): contain job panics at the task boundary —
            // the panicking job is marked failed (with backoff) and the
            // loop claims the next job instead of unwinding the worker's
            // timer task (or aborting the whole app, pre-unwind).
            if AssertUnwindSafe(self.execute_job(job.id, &job.kind, &job.payload))
                .catch_unwind()
                .await
                .is_err()
            {
                tracing::error!(
                    job_id = job.id,
                    kind = %job.kind,
                    "job panicked (contained at the worker boundary): marking failed"
                );
                let conn = self.lock();
                let _ = jobs::fail(&conn, job.id, "job panicked (contained at worker boundary)");
            }
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
                // ---------- ai queue (WK-03 / C6: the automation engine
                // enqueues these; they used to fall to `Unknown job type`
                // and fail permanently). Handlers run the real pipeline;
                // with AI disabled the job completes with a trace note (the
                // automation engine gates its own enqueue sites). ----------
                "analyze_ticket" => {
                    let Some(conv_id) = num("conversationId") else {
                        let conn = self.lock();
                        jobs::fail(&conn, job_id, "analyze_ticket missing conversationId")?;
                        return Ok(());
                    };
                    self.run_ai_job(kind, conv_id, AiJob::Analyze).await?;
                }
                "generate_draft" => {
                    let Some(conv_id) = num("conversationId") else {
                        let conn = self.lock();
                        jobs::fail(&conn, job_id, "generate_draft missing conversationId")?;
                        return Ok(());
                    };
                    self.run_ai_job(kind, conv_id, AiJob::Draft).await?;
                }
                "create_ai_note" => {
                    let Some(conv_id) = num("conversationId") else {
                        let conn = self.lock();
                        jobs::fail(&conn, job_id, "create_ai_note missing conversationId")?;
                        return Ok(());
                    };
                    self.run_ai_job(kind, conv_id, AiJob::Note).await?;
                }
                // ---------- attachments queue (WK-03 / C6) ----------
                "download_recent_attachments" => {
                    self.run_attachment_downloads().await;
                }
                // ---------- api queue: bulk actions (WK-03 / C6 — one job
                // per conversation, `bulk_{action}` kinds, priority 1) ----------
                k if k.starts_with("bulk_") => {
                    let action = k.trim_start_matches("bulk_");
                    if let Err(e) = self.execute_bulk_action(action, &payload).await {
                        let msg = e.to_string();
                        let conn = self.lock();
                        let _ = jobs::fail(&conn, job_id, &msg);
                        let _ = jobs::log_error(
                            &conn,
                            "workers",
                            &format!("Job {kind} failed: {msg}"),
                        );
                        return Ok(());
                    }
                }
                // ---------- demo simulate jobs (demo.rs enqueue kinds) ----------
                // The simulated CSAT rating is PERSISTED (no fabrication: the
                // payload is clearly marked simulated; the ratings row keeps
                // the raw payload in raw_json).
                "rating.process" => {
                    self.persist_simulated_rating(&payload)?;
                }
                // The simulated incoming message cannot be fabricated into
                // the mirror (it exists nowhere on the provider); the real
                // work this job can do is a conversations sync pass.
                "sync.conversations" => {
                    self.run_engine(|e| async move { e.incremental_sync().await.map(|_| ()) })
                        .await?;
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

    // ------------------------------------------------------------------
    // WK-03 (C6): the job-kind handlers that used to fall to `Unknown job
    // type` and fail permanently.
    // ------------------------------------------------------------------

    /// The AI-queue job family (ai queue: analyze_ticket / generate_draft /
    /// create_ai_note). Runs the pipeline with the DB guard confined to a
    /// blocking thread (the AppState::run_ai pattern) so the global mutex is
    /// never held across the LM Studio call from the worker side either.
    async fn run_ai_job(&self, kind: &str, conv_id: i64, job: AiJob) -> Result<()> {
        let backend = {
            let conn = self.lock();
            crate::ai_pipeline::backend_from_settings(&conn)
        };
        if matches!(backend, crate::ai_pipeline::AiBackend::Disabled) {
            // AI off: return without AI output (the outer flow completes the
            // job). Nothing is fabricated; the automation engine gates its
            // own enqueue sites, so this only fires for jobs enqueued
            // elsewhere (manual/demo).
            tracing::info!(
                operation = kind,
                conversation_id = conv_id,
                "AI backend disabled - job completed without AI output"
            );
            return Ok(());
        }
        let conn_arc = self.conn.clone();
        let (result, note_text, remote_id) = tokio::task::spawn_blocking(move || {
            let conn = conn_arc.lock().unwrap_or_else(|p| p.into_inner());
            tokio::runtime::Handle::current().block_on(async {
                // Every AI call site must be able to find the conversation.
                let remote: Option<i64> = conn
                    .query_row(
                        "SELECT remote_id FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
                        rusqlite::params![conv_id],
                        |r| r.get(0),
                    )
                    .ok();
                let remote_id = match remote {
                    Some(r) => r,
                    None => {
                        return (
                            Err(Error::Other("Conversation not found locally.".into())),
                            None,
                            None,
                        );
                    }
                };
                match job {
                    AiJob::Analyze => {
                        let r = crate::ai_pipeline::analyze_ticket(&conn, &backend, conv_id, false)
                            .await
                            .map(|_| ())
                            .map_err(|e| Error::Other(e.to_string().into()));
                        (r, None, Some(remote_id))
                    }
                    AiJob::Draft => {
                        let mode = "standard".to_string();
                        let r = crate::ai_pipeline::generate_draft(
                            &conn, &backend, conv_id, &mode, false, None,
                        )
                        .await
                        .map(|_| ())
                        .map_err(|e| Error::Other(e.to_string().into()));
                        (r, None, Some(remote_id))
                    }
                    AiJob::Note => {
                        // The note body comes from the ticket analysis (the
                        // AI-derived facts); nothing is fabricated.
                        let analysis = match crate::ai_pipeline::analyze_ticket(
                            &conn, &backend, conv_id, false,
                        )
                        .await
                        {
                            Ok(outcome) => outcome.analysis,
                            Err(e) => {
                                return (
                                    Err(Error::Other(e.to_string().into())),
                                    None,
                                    Some(remote_id),
                                );
                            }
                        };
                        let mut body = String::new();
                        if let Some(summary) = &analysis.summary {
                            body.push_str(&format!("Summary: {summary}\n"));
                        }
                        if let Some(intent) = &analysis.intent {
                            body.push_str(&format!("Intent: {intent}\n"));
                        }
                        if let Some(goal) = &analysis.customer_goal {
                            body.push_str(&format!("Customer goal: {goal}\n"));
                        }
                        if let Some(sentiment) = &analysis.sentiment {
                            body.push_str(&format!("Sentiment: {sentiment}"));
                            if let Some(urgency) = &analysis.urgency {
                                body.push_str(&format!(" (urgency: {urgency})"));
                            }
                            body.push('\n');
                        }
                        if !analysis.missing_information.is_empty() {
                            body.push_str(&format!(
                                "Missing information: {}\n",
                                analysis.missing_information.join(", ")
                            ));
                        }
                        if body.trim().is_empty() {
                            return (
                                Err(Error::Other(
                                    "Ticket analysis carried no note content.".into(),
                                )),
                                None,
                                Some(remote_id),
                            );
                        }
                        body.push_str("\n(This note was drafted by SupportOS AI.)");
                        (Ok(()), Some(body), Some(remote_id))
                    }
                }
            })
        })
        .await
        .map_err(|e| Error::Other(e.to_string().into()))?;

        result?;
        // create_ai_note: write the note through the provider boundary, then
        // re-sync the conversation so the local mirror carries the new note.
        if let (Some(text), Some(remote_id)) = (note_text, remote_id) {
            self.provider
                .create_note_thread(crate::helpscout::CreateThreadInput {
                    conversation_id: remote_id,
                    text,
                    draft: false,
                    cc: vec![],
                    bcc: vec![],
                    status_after: None,
                    assign_to: None,
                })
                .await?;
            let engine = self.engine.clone();
            if let Some(engine) = engine {
                engine.sync_single_conversation(remote_id).await?;
            }
            let conn = self.lock();
            let _ = crate::jobs::audit(
                &conn,
                "ai",
                "note_added",
                crate::sync_engine::conversation_local_id(&conn, remote_id),
                None,
                None,
                None,
                None,
                true,
            );
            self.emit_conversation_updated_for_remote(remote_id);
        }
        tracing::info!(
            operation = kind,
            conversation_id = conv_id,
            "AI job completed"
        );
        Ok(())
    }

    /// `download_recent_attachments` (attachments queue): download up to 100
    /// pending attachment rows. The fake provider's downloads land with
    /// deterministic bytes; the real provider has no attachment-fetch support
    /// yet (the SY-10 provider gap) — those rows stay pending and the job
    /// completes with a trace note instead of fabricating content.
    async fn run_attachment_downloads(&self) {
        let ids: Vec<i64> = {
            let conn = self.lock();
            let Ok(mut stmt) = conn.prepare(
                "SELECT id FROM attachments
                  WHERE state IS NULL OR state NOT IN ('downloaded')
                  ORDER BY id DESC LIMIT 100",
            ) else {
                return;
            };
            stmt.query_map([], |r| r.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default()
        };
        if ids.is_empty() {
            return;
        }
        if self.provider.kind() != "fake" {
            tracing::info!(
                count = ids.len(),
                "Attachment download skipped: the real provider does not support attachment fetch yet"
            );
            return;
        }
        let attachments_dir = self.data_dir.join("attachments");
        let mut downloaded = 0usize;
        for id in ids {
            let conn = self.lock();
            match crate::conversation_ops::download_attachment_to(&conn, &attachments_dir, id) {
                Ok(_) => downloaded += 1,
                Err(msg) => {
                    tracing::warn!(attachment_id = id, error = %msg, "Attachment download failed");
                }
            }
        }
        tracing::info!(count = downloaded, "Recent attachments downloaded");
    }

    /// `bulk_{action}` jobs (api queue, one per conversation): execute the
    /// action against the provider + local mirror. `tag`/`untag` are local
    /// mirror writes (the provider trait has no tag-write method — the SY-10
    /// gap, same as the automation add_tag path).
    async fn execute_bulk_action(&self, action: &str, payload: &serde_json::Value) -> Result<()> {
        use crate::helpscout::ConversationPatch;
        let Some(conv_id) = payload.get("conversationId").and_then(|v| v.as_i64()) else {
            return Err(Error::Other("bulk job missing conversationId".into()));
        };
        let row: Option<(i64, String)> = {
            let conn = self.lock();
            conn.query_row(
                "SELECT remote_id, status FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![conv_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
        };
        let Some((remote_id, _current_status)) = row else {
            return Err(Error::Other("Conversation not found locally.".into()));
        };
        match action {
            "tag" | "untag" => {
                let Some(tag) = payload.get("tag").and_then(|v| v.as_str()) else {
                    return Err(Error::Other(format!("bulk_{action} missing tag").into()));
                };
                {
                    let conn = self.lock();
                    let mut tags = crate::conversation_ops::read_conversation_tags(&conn, conv_id);
                    if action == "tag" {
                        if !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                            tags.push(tag.to_string());
                        }
                    } else {
                        tags.retain(|t| !t.eq_ignore_ascii_case(tag));
                    }
                    crate::conversation_ops::write_conversation_tags(&conn, conv_id, &tags);
                }
            }
            "assign" | "unassign" | "status" | "close" => {
                let patch = match action {
                    "close" => ConversationPatch {
                        status: Some("closed".into()),
                        ..Default::default()
                    },
                    "status" => {
                        let Some(status) = payload.get("status").and_then(|v| v.as_str()) else {
                            return Err(Error::Other("bulk_status missing status".into()));
                        };
                        const STATUSES: [&str; 4] = ["active", "closed", "pending", "spam"];
                        if !STATUSES.contains(&status) {
                            return Err(Error::Other(
                                format!("bulk_status invalid status {status}").into(),
                            ));
                        }
                        ConversationPatch {
                            status: Some(status.to_string()),
                            ..Default::default()
                        }
                    }
                    "assign" => {
                        // assignRequestSchema: userId is a REMOTE user id.
                        let Some(user_remote) = payload.get("userId").and_then(|v| v.as_i64())
                        else {
                            return Err(Error::Other("bulk_assign missing userId".into()));
                        };
                        ConversationPatch {
                            assign_to: Some(Some(user_remote)),
                            ..Default::default()
                        }
                    }
                    _ => ConversationPatch {
                        // unassign: Some(None) = clear assignee.
                        assign_to: Some(None),
                        ..Default::default()
                    },
                };
                self.provider.update_conversation(remote_id, patch).await?;
                // Local mirror write (checked transaction — the C2 pattern).
                {
                    let mut conn = self.lock();
                    let assignee_local = if action == "assign" {
                        payload
                            .get("userId")
                            .and_then(|v| v.as_i64())
                            .and_then(|user_remote| {
                                // The users mirror may not have seen this
                                // user yet; the remote write succeeded and
                                // the next full sync reconciles the local
                                // row (logged when unresolvable).
                                let local =
                                    crate::sync_engine::local_id(&conn, "users", user_remote);
                                if local.is_none() {
                                    tracing::warn!(
                                        conversation_id = conv_id,
                                        user_remote,
                                        "bulk_assign: user not in local mirror; local assignee left unchanged"
                                    );
                                }
                                local
                            })
                    } else {
                        None
                    };
                    let tx = conn.transaction()?;
                    if let Some(status) = payload
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| (action == "close").then(|| "closed".to_string()))
                        .filter(|s| !s.is_empty())
                    {
                        tx.execute(
                            "UPDATE conversations SET status = ?1,
                                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                               WHERE id = ?2",
                            rusqlite::params![status, conv_id],
                        )?;
                        if status == "closed" {
                            tx.execute(
                                "UPDATE conversations SET closed_at = COALESCE(closed_at, datetime('now'))
                                   WHERE id = ?1",
                                rusqlite::params![conv_id],
                            )?;
                        }
                    }
                    if action == "assign" {
                        if let Some(local) = assignee_local {
                            tx.execute(
                                "UPDATE conversations SET assignee_id = ?1 WHERE id = ?2",
                                rusqlite::params![local, conv_id],
                            )?;
                        }
                    }
                    if action == "unassign" {
                        tx.execute(
                            "UPDATE conversations SET assignee_id = NULL WHERE id = ?1",
                            rusqlite::params![conv_id],
                        )?;
                    }
                    tx.commit()?;
                }
            }
            _ => {
                return Err(Error::Other(
                    format!("Unknown bulk action: {action}").into(),
                ));
            }
        }
        {
            let conn = self.lock();
            let _ = crate::jobs::audit(
                &conn,
                "user",
                &format!("bulk_{action}"),
                Some(conv_id),
                None,
                None,
                None,
                None,
                false,
            );
        }
        self.emit_conversation_updated_for_remote(remote_id);
        Ok(())
    }

    /// `rating.process` (the demo simulate-CSAT job): persist the simulated
    /// rating. The payload is marked simulated; the raw payload is kept in
    /// `raw_json` so nothing masquerades as provider data.
    fn persist_simulated_rating(&self, payload: &serde_json::Value) -> Result<()> {
        let rating_num = payload.get("rating").and_then(|v| v.as_i64()).unwrap_or(5);
        let label = match rating_num {
            4 | 5 => "great",
            3 => "okay",
            _ => "not-good",
        };
        let conv_remote = payload
            .get("conversation_id")
            .and_then(|v| v.as_i64())
            .or_else(|| payload.get("conversationId").and_then(|v| v.as_i64()));
        let comment = payload.get("comment").and_then(|v| v.as_str());
        let conn = self.lock();
        let conv_local =
            conv_remote.and_then(|r| crate::sync_engine::conversation_local_id(&conn, r));
        // Insert (not upsert): every simulated rating is a distinct event.
        conn.execute(
            "INSERT INTO ratings (conversation_id, rating, comments, raw_json, remote_created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                conv_local,
                label,
                comment,
                payload.to_string(),
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            ],
        )?;
        if let Some(conv_local) = conv_local {
            let _ = crate::jobs::audit(
                &conn,
                "user",
                "rating_simulated",
                Some(conv_local),
                None,
                Some(&format!("{{\"rating\":\"{label}\"}}")),
                None,
                None,
                false,
            );
        }
        tracing::info!(rating = label, "Simulated CSAT rating persisted");
        Ok(())
    }
}

/// The AI-queue job kinds (WK-03).
#[derive(Clone, Copy)]
enum AiJob {
    Analyze,
    Draft,
    Note,
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

    // ---- C3: panic containment at the task boundary -----------------------

    #[tokio::test]
    async fn contained_swallows_panicking_tick_and_keeps_going() {
        // A panicking timer body must be contained — the helper returns
        // normally instead of unwinding the caller (the timer loop).
        WorkerManager::contained("test_boom", async {
            panic!("timer body boom");
        })
        .await;
        // And the very next tick runs fine afterwards.
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ran2 = ran.clone();
        WorkerManager::contained("test_ok", async move {
            ran2.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tick_marks_a_panicking_job_failed_and_survives() {
        // The tick loop's post-panic recovery contract (C3): after a job
        // body panics and the catch_unwind in tick() contains it, the
        // follow-up jobs::fail must dead-letter the claimed job. Drive it
        // against a real DB with a job in the exact 'running' state a
        // claimed job has.
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut conn = Connection::open(tmp.path().join("t.db")).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        let shared = Arc::new(Mutex::new(conn));
        let bus = EventBus::default();
        let provider = Arc::new(crate::helpscout::FakeHelpScoutProvider::new_demo())
            as Arc<dyn HelpScoutProvider>;
        let engine = Arc::new(crate::sync_engine::SyncEngine::new(
            shared.clone(),
            provider.clone(),
        ));
        let manager = WorkerManager::new(
            shared.clone(),
            Some(engine),
            provider,
            bus,
            tmp.path().to_path_buf(),
            None,
        );

        // A claimed job to dead-letter.
        jobs::enqueue_on(&shared.lock().unwrap(), "api", "boom_kind", "{}", 1).unwrap();
        let (job_id,) = {
            let c = shared.lock().unwrap();
            c.query_row(
                "SELECT id FROM jobs WHERE type = 'boom_kind' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get::<_, i64>(0)?,)),
            )
            .unwrap()
        };
        // Claimed jobs run as 'running' — the state the tick loop sees when
        // the body panics mid-execution. max_attempts = 1 so the post-panic
        // jobs::fail dead-letters immediately (attempt >= max_attempts);
        // with attempts left it would be requeued with backoff instead.
        {
            let c = shared.lock().unwrap();
            c.execute(
                "UPDATE jobs SET status = 'running', attempt = 1, max_attempts = 1 WHERE id = ?1",
                rusqlite::params![job_id],
            )
            .unwrap();
        }
        // (Silence the panic hook so the expected panic is not printed
        // into the test output.)
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = AssertUnwindSafe(async { panic!("job body boom") })
            .catch_unwind()
            .await
            .is_err();
        std::panic::set_hook(prev_hook);
        assert!(panicked);
        {
            let c = shared.lock().unwrap();
            jobs::fail(&c, job_id, "job panicked (contained at worker boundary)").unwrap();
        }
        let status: String = {
            let c = shared.lock().unwrap();
            c.query_row(
                "SELECT status FROM jobs WHERE id = ?1",
                rusqlite::params![job_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        // attempt 1 of max_attempts 1 -> dead-lettered, ready for the
        // stale-job sweep instead of an aborted process.
        assert_eq!(status, "failed");
        assert!(!manager.is_running(), "manager not started in this test");
    }

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

    // ---- WK-03 (C6): every enqueued job kind is now runnable -------------

    /// A worker wired like production: the shared conn, the sync engine and
    /// the fake provider (one world), a throwaway data dir.
    async fn demo_worker(
        db: &std::path::Path,
        data_dir: &std::path::Path,
    ) -> (Arc<Mutex<Connection>>, WorkerManager) {
        let mut conn = crate::db::open(db).expect("open DB");
        crate::bootstrap::apply_all(&mut conn).expect("apply all migrations");
        let shared = Arc::new(Mutex::new(conn));
        let provider = Arc::new(crate::helpscout::FakeHelpScoutProvider::new_demo())
            as Arc<dyn crate::helpscout::HelpScoutProvider>;
        let engine = Arc::new(SyncEngine::new(shared.clone(), provider.clone()));
        engine.initial_sync().await.expect("demo initial sync");
        let manager = WorkerManager::new(
            shared.clone(),
            Some(engine),
            provider,
            EventBus::new(64),
            data_dir.to_path_buf(),
            None,
        );
        (shared, manager)
    }

    /// Enqueue + claim (the tick's transition) + execute one job; returns the
    /// job id so the caller can assert its final state. NEVER holds the DB
    /// guard across the execute (the handlers lock it themselves).
    async fn run_job(
        shared: &Arc<Mutex<Connection>>,
        manager: &WorkerManager,
        queue: &str,
        kind: &str,
        payload: &str,
    ) -> i64 {
        let id = {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            let id = jobs::enqueue_on(&conn, queue, kind, payload, 2).expect("enqueue");
            conn.execute(
                "UPDATE jobs SET status = 'running', attempt = 1 WHERE id = ?1",
                rusqlite::params![id],
            )
            .expect("claim");
            id
        };
        manager.execute_job(id, kind, payload).await;
        id
    }

    fn job_status(shared: &Arc<Mutex<Connection>>, id: i64) -> String {
        let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT status FROM jobs WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .expect("job row")
    }

    fn col(shared: &Arc<Mutex<Connection>>, sql: &str, conv: i64) -> Option<String> {
        let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(sql, rusqlite::params![conv], |r| r.get(0))
            .ok()
            .flatten()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bulk_actions_execute_against_provider_and_mirror() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (shared, manager) = demo_worker(&tmp.path().join("wk03.db"), tmp.path()).await;

        let (conv, remote): (i64, i64) = {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT id, remote_id FROM conversations WHERE deleted_at IS NULL
                  ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };

        // bulk_close: provider write + local mirror (status + closed_at).
        let id = run_job(
            &shared,
            &manager,
            "api",
            "bulk_close",
            &format!("{{\"conversationId\":{conv}}}"),
        )
        .await;
        assert_eq!(job_status(&shared, id), "completed", "bulk_close completed");
        assert_eq!(
            col(
                &shared,
                "SELECT status FROM conversations WHERE id = ?1",
                conv
            )
            .as_deref(),
            Some("closed"),
            "local status closed"
        );
        assert!(
            col(
                &shared,
                "SELECT closed_at FROM conversations WHERE id = ?1",
                conv
            )
            .is_some(),
            "closed_at stamped"
        );

        // bulk_status with an invalid status fails the job (never ok:true).
        let id = run_job(
            &shared,
            &manager,
            "api",
            "bulk_status",
            &format!("{{\"conversationId\":{conv},\"status\":\"bogus\"}}"),
        )
        .await;
        assert_ne!(
            job_status(&shared, id),
            "completed",
            "invalid status rejected"
        );

        // bulk_status pending.
        let id = run_job(
            &shared,
            &manager,
            "api",
            "bulk_status",
            &format!("{{\"conversationId\":{conv},\"status\":\"pending\"}}"),
        )
        .await;
        assert_eq!(job_status(&shared, id), "completed");
        assert_eq!(
            col(
                &shared,
                "SELECT status FROM conversations WHERE id = ?1",
                conv
            )
            .as_deref(),
            Some("pending")
        );

        // bulk_tag / bulk_untag (local mirror writes, the add_tag path).
        let id = run_job(
            &shared,
            &manager,
            "api",
            "bulk_tag",
            &format!("{{\"conversationId\":{conv},\"tag\":\"vip\"}}"),
        )
        .await;
        assert_eq!(job_status(&shared, id), "completed");
        {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            let tags = crate::conversation_ops::read_conversation_tags(&conn, conv);
            assert!(
                tags.iter().any(|t| t.eq_ignore_ascii_case("vip")),
                "{tags:?}"
            );
        }
        let id = run_job(
            &shared,
            &manager,
            "api",
            "bulk_untag",
            &format!("{{\"conversationId\":{conv},\"tag\":\"vip\"}}"),
        )
        .await;
        assert_eq!(job_status(&shared, id), "completed");
        {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            let tags = crate::conversation_ops::read_conversation_tags(&conn, conv);
            assert!(
                !tags.iter().any(|t| t.eq_ignore_ascii_case("vip")),
                "{tags:?}"
            );
        }

        // bulk_assign: remote user 1001 (demo world) -> local assignee.
        let user_local: i64 = {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row("SELECT id FROM users WHERE remote_id = 1001", [], |r| {
                r.get(0)
            })
            .expect("demo user mirrored")
        };
        let id = run_job(
            &shared,
            &manager,
            "api",
            "bulk_assign",
            &format!("{{\"conversationId\":{conv},\"userId\":1001}}"),
        )
        .await;
        assert_eq!(job_status(&shared, id), "completed");
        assert_eq!(
            col(
                &shared,
                "SELECT CAST(assignee_id AS TEXT) FROM conversations WHERE id = ?1",
                conv
            )
            .as_deref(),
            Some(user_local.to_string().as_str()),
            "local assignee resolved to the demo user"
        );

        // bulk_unassign clears it.
        let id = run_job(
            &shared,
            &manager,
            "api",
            "bulk_unassign",
            &format!("{{\"conversationId\":{conv}}}"),
        )
        .await;
        assert_eq!(job_status(&shared, id), "completed");
        assert!(
            col(
                &shared,
                "SELECT CAST(assignee_id AS TEXT) FROM conversations WHERE id = ?1",
                conv
            )
            .is_none(),
            "assignee cleared"
        );

        // Every bulk job also wrote an audit row.
        let audits: i64 = {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'bulk_%' AND conversation_id = ?1",
                rusqlite::params![conv],
                |r| r.get(0),
            )
            .unwrap_or(0)
        };
        assert!(audits >= 6, "bulk actions audited: {audits}");
        let _ = remote;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attachment_download_job_downloads_pending_rows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (shared, manager) = demo_worker(&tmp.path().join("wk03-att.db"), tmp.path()).await;
        {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            let conv: i64 = conn
                .query_row(
                    "SELECT id FROM conversations WHERE deleted_at IS NULL ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO attachments (remote_id, conversation_id, filename, mime_type, size, state)
                 VALUES (5001, ?1, 'invoice.pdf', 'application/pdf', 1234, 'metadata')",
                rusqlite::params![conv],
            )
            .unwrap();
        }

        let id = run_job(
            &shared,
            &manager,
            "attachments",
            "download_recent_attachments",
            "{}",
        )
        .await;
        assert_eq!(job_status(&shared, id), "completed", "the job is runnable");
        let (state, path, hash): (String, Option<String>, Option<String>) = {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT state, local_path, hash FROM attachments WHERE remote_id = 5001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        };
        assert_eq!(state, "downloaded");
        let path = path.expect("local path recorded");
        assert!(std::path::Path::new(&path).exists(), "file on disk: {path}");
        assert!(
            hash.as_deref().is_some_and(|h| h.len() == 64),
            "sha256 recorded"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rating_process_persists_the_simulated_rating() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (shared, manager) = demo_worker(&tmp.path().join("wk03-rating.db"), tmp.path()).await;
        let (conv_local, conv_remote): (i64, i64) = {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT id, remote_id FROM conversations WHERE deleted_at IS NULL ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        let payload = serde_json::json!({
            "id": "demo_rating_xyz", "conversation_id": conv_remote,
            "rating": 5, "comment": "Great support!", "simulated": true
        })
        .to_string();

        let id = run_job(&shared, &manager, "sync", "rating.process", &payload).await;
        assert_eq!(job_status(&shared, id), "completed", "the job is runnable");
        let (rating, comments, raw, conv): (String, Option<String>, String, i64) = {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT rating, comments, raw_json, conversation_id FROM ratings
                  WHERE raw_json LIKE '%demo_rating_xyz%'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("simulated rating persisted")
        };
        assert_eq!(rating, "great", "5 maps to the reference vocabulary");
        assert_eq!(comments.as_deref(), Some("Great support!"));
        assert!(
            raw.contains("\"simulated\":true"),
            "raw payload kept: {raw}"
        );
        assert_eq!(conv, conv_local, "resolved to the local conversation");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ai_jobs_complete_without_output_when_backend_disabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (shared, manager) = demo_worker(&tmp.path().join("wk03-ai.db"), tmp.path()).await;
        let (conv, threads_before): (i64, i64) = {
            let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
            crate::settings::set_bool(&conn, "ai_enabled", false).unwrap();
            let conv = conn
                .query_row(
                    "SELECT id FROM conversations WHERE deleted_at IS NULL ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let threads = conn
                .query_row("SELECT COUNT(*) FROM conversation_threads", [], |r| {
                    r.get(0)
                })
                .unwrap();
            (conv, threads)
        };

        // The three ai-queue kinds the automation engine enqueues: with the
        // backend disabled they COMPLETE (no permanent failures, no
        // fabricated AI output).
        for kind in ["analyze_ticket", "generate_draft", "create_ai_note"] {
            let id = run_job(
                &shared,
                &manager,
                "ai",
                kind,
                &format!("{{\"conversationId\":{conv}}}"),
            )
            .await;
            assert_eq!(
                job_status(&shared, id),
                "completed",
                "{kind} completes with AI disabled"
            );
        }
        let conn = shared.lock().unwrap_or_else(|p| p.into_inner());
        let runs: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(runs, 0, "no fabricated AI runs");
        let threads_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_threads", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(threads_before, threads_after, "no fabricated notes");
    }
}
