//! SupportOS++ — Tauri 2 desktop shell.
//!
//! Boot order:
//!   1. Init logging (JSON in release, pretty in dev).
//!   2. Load AppConfig (defaults; data_dir from SPP_DATA_DIR or per-OS convention).
//!   3. Open SQLite with ALL migrations (M001–M028) applied.
//!   4. Bind the loopback listener (D-002) on a free 127.0.0.1 port.
//!   5. Start the HTTP API server on 127.0.0.1:3000 (mirrors the reference's Fastify server).
//!   6. Start the Tauri app with IPC commands wired to the Rust core.
//!
//! The HTTP server exposes all 310 API routes (same as the reference's Fastify server),
//! serving both the Tauri webview and any browser client on localhost.
//! Webhooks, SSE events, and demo endpoints are served via the HTTP server.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use std::sync::{Arc, Mutex};

use spp_core::config::AppConfig;
use spp_core::db;

/// Entry point called by `main.rs`.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    spp_core::logging::init();

    tracing::info!("SupportOS++ starting");

    // 1. Load the config (data_dir from SPP_DATA_DIR or per-OS convention).
    let app_config = AppConfig::default();
    tracing::info!(?app_config.data_dir, "data directory");

    // 2. Open the SQLite DB with migrations. The connection is wrapped in a
    //    Mutex and managed by Tauri's state system so IPC commands can access it.
    let db_path = app_config.data_dir.join("supportos-plusplus.db");
    let conn = match open_db_with_all_migrations(&db_path) {
        Ok(c) => {
            tracing::info!(path = %db_path.display(), "SQLite DB opened with all migrations M001–M028");
            c
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to open SQLite DB; falling back to in-memory state");
            return launch_without_db();
        }
    };

    // 3. Bind the loopback listener (A2) — for OAuth callback + webhook receiver.
    let loopback_addr = match tauri::async_runtime::block_on(spp_core::loopback::Loopback::bind(
        app_config.loopback_port,
    )) {
        Ok(l) => {
            let addr = l.local_addr();
            tracing::info!(%addr, "loopback listener bound");
            Some(addr)
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to bind loopback listener");
            None
        }
    };
    let loopback_addr_str = loopback_addr.map(|a| a.to_string());

    // 3.5 Run the startup self-check.
    let self_check_report = spp_core::self_check::run(&conn, loopback_addr_str.clone())
        .map_err(|e| {
            tracing::error!(error = %e, "self-check failed to run");
            e.to_string()
        })
        .ok();
    if let Some(ref report) = self_check_report {
        tracing::info!(?report, "startup self-check report");
        if !report.all_ok {
            tracing::warn!("startup self-check found failing subsystems — see report");
        }
    }

    // 4. Start the HTTP API server on 127.0.0.1:3000 (mirrors the reference's Fastify server).
    let http_port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let demo_mode = spp_core::settings::get_bool(&conn, "demo_mode", false).unwrap_or(false)
        || std::env::var("LOCAL_DEMO_MODE").as_deref() == Ok("true");

    // Open a SEPARATE connection for the HTTP server (SQLite WAL supports
    // multiple connections to the same file). This avoids sharing a Mutex
    // between the HTTP server's async handlers and Tauri's IPC commands.
    let http_conn = match open_db_with_all_migrations(&db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to open HTTP server DB connection; HTTP API will be unavailable");
            rusqlite::Connection::open_in_memory().expect("in-memory fallback")
        }
    };

    let http_conn = Arc::new(Mutex::new(http_conn));
    let http_bus = spp_core::http::EventBus::default();

    // Provider selection (reference context.ts): demo mode -> Fake, else Real
    // with env credentials (HELPSCOUT_CLIENT_ID / _SECRET / _REDIRECT_URI).
    let credentials = spp_core::helpscout_real::HsCredentials::from_env();
    let use_real = !demo_mode && credentials.is_configured();
    let (provider, real, provider_kind): (
        Arc<dyn spp_core::helpscout::HelpScoutProvider>,
        Option<Arc<spp_core::helpscout_real::RealHelpScoutProvider>>,
        String,
    ) = if use_real {
        let real = Arc::new(spp_core::helpscout_real::RealHelpScoutProvider::new(
            http_conn.clone(),
            credentials,
        ));
        (
            real.clone() as Arc<dyn spp_core::helpscout::HelpScoutProvider>,
            Some(real),
            "real".to_string(),
        )
    } else {
        (
            Arc::new(spp_core::helpscout::FakeHelpScoutProvider::new_demo())
                as Arc<dyn spp_core::helpscout::HelpScoutProvider>,
            None,
            "fake".to_string(),
        )
    };
    let sync = Arc::new(
        spp_core::sync_engine::SyncEngine::new(http_conn.clone(), provider)
            .with_bus(http_bus.clone()),
    );

    let http_state = spp_core::http::server::AppState {
        conn: http_conn,
        data_dir: app_config.data_dir.clone(),
        port: http_port,
        host: "127.0.0.1".to_string(),
        demo_mode,
        bus: http_bus,
        limiter: spp_core::http::RateLimiter::new(),
        sync: Some(sync),
        real,
        provider_kind,
        workers: None,
    };

    let http_server = spp_core::http::HttpServer::new(http_state);
    let http_addr = http_server.addr();
    tracing::info!(%http_addr, "HTTP API server starting (mirrors reference Fastify on :3000)");

    // Spawn the HTTP server as a background task.
    tauri::async_runtime::spawn(async move {
        if let Err(e) = http_server.serve().await {
            tracing::error!(error = %e, "HTTP API server failed");
        }
    });

    // 5. Launch Tauri with the DB connection in state + all IPC commands.
    let db_state = DbState {
        conn: Mutex::new(conn),
        self_check_report,
    };

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {
            // When a second instance is launched, focus the existing window.
        }))
        .manage(db_state)
        .invoke_handler(tauri::generate_handler![
            // Foundation
            ping,
            version,
            catalog_counts,
            first_run_state,
            // Operations Center (M4-T01)
            operations_snapshot,
            // Notifications (M4-T04)
            notifications_unread_count,
            notifications_list_unread,
            notifications_mark_read,
            // Dashboard (M8-T01)
            dashboard_metrics,
            // AI Center (M6-T01)
            ai_status,
            ai_set_provider,
            ai_set_chat_model,
            ai_set_embedding_model,
            // Copilot allowlist (M6-T01/A10)
            copilot_allowlist,
            // Automation (M4-T10)
            automation_list_rules,
            automation_list_pending,
            automation_approve,
            automation_reject,
            // Sync Health (M2-T05)
            sync_health_state,
            // Reports (M8-T02)
            report_build,
            // Support health (M8-T05)
            support_health,
            // Intelligence — Issue Radar (M7-T04)
            issue_radar_snapshot,
            // Conformance (M11-T01)
            parity_gate_check,
            // Self-check (M12 — packaging verification)
            self_check,
            // Inbox (M12-P5 — conversation list + detail + reply + note + status + assign)
            inbox_list_conversations,
            inbox_get_conversation,
            inbox_reply,
            inbox_add_note,
            inbox_change_status,
            inbox_assign,
            inbox_list_saved_views,
            inbox_apply_saved_view,
            // Customers (M12-P5b — customer profile + history + timeline)
            customer_get,
            customer_conversations,
            customer_timeline,
            customer_search,
            // Intelligence — Incidents (M12-P5h)
            incidents_list,
            // Intelligence — Knowledge gaps (M12-P5h)
            knowledge_gaps_list,
            // Side threads (M12-P5h)
            side_threads_list,
            side_thread_messages,
            // Connectors (M12-P5h)
            connectors_list,
            // Custom objects (M12-P5h)
            custom_object_types_list,
            custom_object_fields_list,
            // Outreach (M12-P5i)
            segments_list,
            campaigns_list,
            dnc_list,
            // Search (M12-P5i)
            universal_search,
            // Backup (M12-P5i)
            backup_export,
            // Support graph (M12-P5j)
            graph_nodes_list,
            graph_neighbors,
        ]);

    #[cfg(desktop)]
    {
        builder = builder.setup(|_app| {
            tracing::info!("SupportOS++ Tauri shell ready (M11 — all IPC commands wired)");
            Ok(())
        });
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running SupportOS++");
}

/// Open the DB and apply ALL migrations (M001 base + M002 sync tables + M003–M028).
/// This is the single entry point for DB initialization at boot.
fn open_db_with_all_migrations(
    path: &std::path::Path,
) -> spp_core::error::Result<rusqlite::Connection> {
    let mut conn = db::open(path)?;
    db::ensure_migrations_table(&conn)?;
    spp_core::migrations::run_all(&mut conn)?;

    // Apply all incremental migrations (M003–M028).
    spp_core::activity::apply_m003(&conn)?;
    spp_core::ticket_states::apply_m004(&conn)?;
    spp_core::notifications::apply_m005(&conn)?;
    spp_core::side_threads::apply_m006(&conn)?;
    spp_core::automation::apply_m007(&conn)?;
    spp_core::embeddings::apply_m008(&conn)?;
    spp_core::ai_center::apply_m009(&conn)?;
    spp_core::ai_analysis::apply_m010(&conn)?;
    spp_core::ai_features::apply_m011_to_m013(&conn)?;
    spp_core::intelligence::apply_m014(&conn)?;
    spp_core::intelligence_features::apply_m015_to_m019(&conn)?;
    spp_core::reports::apply_m020_to_m022(&conn)?;
    spp_core::outreach::apply_m023_to_m025(&conn)?;
    spp_core::data_tools::apply_m026_to_m027(&conn)?;
    spp_core::inbox::apply_m028(&conn)?;
    spp_core::conversation_ops::apply_m030(&conn)?;
    spp_core::outreach::apply_m031(&conn)?;
    spp_core::ticket_states::apply_m032(&conn)?;
    spp_core::ai_attributes::apply_m033(&conn)?;
    spp_core::intelligence_features::apply_m035(&conn)?;
    spp_core::customer_events::apply_m036(&conn)?;
    spp_core::maintenance::apply_m037(&conn)?;
    spp_core::connectors::apply_m038(&conn)?;
    spp_core::db_breadth::apply_m040(&conn)?;

    tracing::info!("All migrations M001–M035 applied successfully");
    Ok(conn)
}

/// Launch the app without a DB connection (fallback for disk-full etc.).
fn launch_without_db() {
    // Still start the HTTP server with an in-memory DB so the API is available.
    let http_port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let demo_mode = std::env::var("LOCAL_DEMO_MODE").as_deref() == Ok("true");

    let http_conn = rusqlite::Connection::open_in_memory()
        .expect("failed to open in-memory SQLite connection for HTTP server");
    let _ = spp_core::db::ensure_migrations_table(&http_conn);
    let mut http_conn = http_conn;
    let _ = spp_core::migrations::run_all(&mut http_conn);

    let http_conn = Arc::new(Mutex::new(http_conn));
    let http_bus = spp_core::http::EventBus::default();
    let provider: Arc<dyn spp_core::helpscout::HelpScoutProvider> =
        Arc::new(spp_core::helpscout::FakeHelpScoutProvider::new_demo());
    let sync = Arc::new(
        spp_core::sync_engine::SyncEngine::new(http_conn.clone(), provider)
            .with_bus(http_bus.clone()),
    );
    let http_state = spp_core::http::server::AppState {
        conn: http_conn,
        data_dir: std::env::temp_dir(),
        port: http_port,
        host: "127.0.0.1".to_string(),
        demo_mode,
        bus: http_bus,
        limiter: spp_core::http::RateLimiter::new(),
        sync: Some(sync),
        real: None,
        provider_kind: "fake".to_string(),
        workers: None,
    };

    let http_server = spp_core::http::HttpServer::new(http_state);
    tracing::info!(addr = %http_server.addr(), "HTTP API server starting (in-memory fallback)");

    tauri::async_runtime::spawn(async move {
        if let Err(e) = http_server.serve().await {
            tracing::error!(error = %e, "HTTP API server failed");
        }
    });

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .manage(DbState {
            conn: Mutex::new(fallback_in_memory_conn()),
            self_check_report: None,
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            version,
            catalog_counts,
            self_check,
        ]);

    #[cfg(desktop)]
    {
        builder = builder.setup(|_app| {
            tracing::error!(
                "SupportOS++ launched WITHOUT a DB connection — IPC commands will fail"
            );
            Ok(())
        });
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running SupportOS++");
}

/// Open an in-memory SQLite connection with the base migrations only.
/// Used as a fallback when the on-disk DB cannot be opened (disk full,
/// permissions, etc.). The self_check will report the degraded state.
fn fallback_in_memory_conn() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap_or_else(|_| {
        // If even an in-memory connection fails, panic — the app cannot run.
        panic!("failed to open in-memory SQLite connection");
    });
    let _ = spp_core::db::ensure_migrations_table(&conn);
    let mut conn_mut = conn;
    let _ = spp_core::migrations::run_all(&mut conn_mut);
    conn_mut
}

/// The shared DB state managed by Tauri. IPC commands access this via
/// `tauri::State<DbState>`.
pub struct DbState {
    /// The SQLite connection. Wrapped in a Mutex for thread safety (Tauri
    /// IPC commands run on a thread pool).
    pub conn: Mutex<rusqlite::Connection>,
    /// The startup self-check report (None if self-check failed to run).
    /// Immutable after boot; read by the `self_check` IPC command.
    pub self_check_report: Option<spp_core::self_check::SelfCheckReport>,
}

impl DbState {
    /// Lock the DB connection for access. Returns a MutexGuard or an error string.
    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>, String> {
        self.conn.lock().map_err(|e| e.to_string())
    }
}

// ─── Foundation IPC commands ──────────────────────────────────────────────

/// Trivial IPC command for smoke-testing the bridge.
#[tauri::command]
fn ping() -> &'static str {
    "pong"
}

/// Returns the application version (matches `Cargo.toml`).
#[tauri::command]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Returns the verified canonical counts (from the catalog enums).
#[tauri::command]
fn catalog_counts() -> serde_json::Value {
    use spp_core::catalog::*;
    serde_json::json!({
        "activity_fields": ActivityField::ALL.len(),
        "date_modes": DateMode::ALL.len(),
        "condition_kinds": ConditionKind::ALL.len(),
        "operations_tiles": OperationsTileKey::ALL.len(),
        "notification_types": NotificationType::ALL.len(),
        "report_metrics": ReportMetricKey::ALL.len(),
        "report_dimensions": ReportDimensionKey::ALL.len(),
        "graph_node_kinds": GraphNodeKind::ALL.len(),
        "ai_attribute_keys": AiAttributeKey::ALL.len(),
        "copilot_tools": CopilotTool::ALL.len(),
    })
}

/// Read or write the first-run onboarding flag (M2-T07 — wired to real DB).
#[tauri::command]
fn first_run_state(
    db_state: tauri::State<'_, DbState>,
    demo_mode: Option<bool>,
) -> Result<bool, String> {
    let conn = db_state.lock_conn()?;

    if let Some(demo) = demo_mode {
        spp_core::settings::mark_first_run_done(&conn).map_err(|e| e.to_string())?;
        if demo {
            spp_core::settings::set_bool(&conn, "demo_mode", true).map_err(|e| e.to_string())?;
        }
    }

    let done = spp_core::settings::first_run_done(&conn).map_err(|e| e.to_string())?;
    Ok(done)
}

// ─── Operations Center (M4-T01) ───────────────────────────────────────────

/// Get the Operations Center snapshot — all 16 tile counts.
/// The UI's `/operations` page calls this on load.
#[tauri::command]
fn operations_snapshot(
    db_state: tauri::State<'_, DbState>,
    mailbox_id: Option<i64>,
) -> Result<serde_json::Value, String> {
    let conn = db_state.lock_conn()?;
    let snapshot =
        spp_core::operations::build_snapshot(&conn, mailbox_id).map_err(|e| e.to_string())?;
    serde_json::to_value(&snapshot).map_err(|e| e.to_string())
}

// ─── Notifications (M4-T04) ──────────────────────────────────────────────

/// Count unread notifications for a user. The UI badge uses this.
#[tauri::command]
fn notifications_unread_count(
    db_state: tauri::State<'_, DbState>,
    user_id: i64,
) -> Result<u32, String> {
    let conn = db_state.lock_conn()?;
    spp_core::notifications::count_unread_for_user(&conn, user_id).map_err(|e| e.to_string())
}

/// List unread notifications for a user (most recent first).
#[tauri::command]
fn notifications_list_unread(
    db_state: tauri::State<'_, DbState>,
    user_id: i64,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let notifications =
        spp_core::notifications::list_unread_for_user(&conn, user_id, limit.unwrap_or(50))
            .map_err(|e| e.to_string())?;
    notifications
        .into_iter()
        .map(|n| serde_json::to_value(&n).map_err(|e| e.to_string()))
        .collect()
}

/// Mark a notification as read.
#[tauri::command]
fn notifications_mark_read(
    db_state: tauri::State<'_, DbState>,
    notification_id: i64,
) -> Result<bool, String> {
    let conn = db_state.lock_conn()?;
    spp_core::notifications::mark_as_read(&conn, notification_id).map_err(|e| e.to_string())
}

// ─── Dashboard (M8-T01) ───────────────────────────────────────────────────

/// Get dashboard KPI metrics.
#[tauri::command]
fn dashboard_metrics(
    db_state: tauri::State<'_, DbState>,
    mailbox_id: Option<i64>,
    days_back: Option<u32>,
) -> Result<serde_json::Value, String> {
    let conn = db_state.lock_conn()?;
    let metrics =
        spp_core::reports::get_dashboard_metrics(&conn, mailbox_id, days_back.unwrap_or(7))
            .map_err(|e| e.to_string())?;
    serde_json::to_value(&metrics).map_err(|e| e.to_string())
}

// ─── AI Center (M6-T01) ───────────────────────────────────────────────────

/// Get the AI status (provider, models, embedding dim).
#[tauri::command]
fn ai_status(db_state: tauri::State<'_, DbState>) -> Result<serde_json::Value, String> {
    let conn = db_state.lock_conn()?;
    let status = spp_core::ai_center::get_ai_status(&conn).map_err(|e| e.to_string())?;
    serde_json::to_value(&status).map_err(|e| e.to_string())
}

/// Set the AI provider kind (None/LmStudio).
#[tauri::command]
fn ai_set_provider(
    db_state: tauri::State<'_, DbState>,
    provider_kind: String,
) -> Result<(), String> {
    let conn = db_state.lock_conn()?;
    let kind = spp_core::ai_center::ProviderKind::parse(&provider_kind)
        .ok_or_else(|| format!("unknown provider kind: {provider_kind}"))?;
    spp_core::ai_center::set_provider_kind(&conn, kind).map_err(|e| e.to_string())
}

/// Set the chat model.
#[tauri::command]
fn ai_set_chat_model(db_state: tauri::State<'_, DbState>, model: String) -> Result<(), String> {
    let conn = db_state.lock_conn()?;
    spp_core::ai_center::set_chat_model(&conn, &model).map_err(|e| e.to_string())
}

/// Set the embedding model + dim.
#[tauri::command]
fn ai_set_embedding_model(
    db_state: tauri::State<'_, DbState>,
    model: String,
    dim: usize,
) -> Result<(), String> {
    let conn = db_state.lock_conn()?;
    spp_core::ai_center::set_embedding_model(&conn, &model, dim).map_err(|e| e.to_string())
}

// ─── Copilot allowlist (M6-T01/A10) ───────────────────────────────────────

/// Returns the Copilot tool allowlist (22 read-only tools) for transparency (A10).
#[tauri::command]
fn copilot_allowlist() -> Vec<serde_json::Value> {
    use spp_core::catalog::CopilotTool;
    CopilotTool::ALL
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.as_str(),
                "description": t.description(),
            })
        })
        .collect()
}

// ─── Automation (M4-T10) ──────────────────────────────────────────────────

/// List all automation rules.
#[tauri::command]
fn automation_list_rules(
    db_state: tauri::State<'_, DbState>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let rules = spp_core::automation::list_rules(&conn).map_err(|e| e.to_string())?;
    rules
        .into_iter()
        .map(|r| serde_json::to_value(&r).map_err(|e| e.to_string()))
        .collect()
}

/// List pending automation approvals.
#[tauri::command]
fn automation_list_pending(
    db_state: tauri::State<'_, DbState>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let approvals =
        spp_core::automation::list_pending_approvals(&conn).map_err(|e| e.to_string())?;
    approvals
        .into_iter()
        .map(|a| serde_json::to_value(&a).map_err(|e| e.to_string()))
        .collect()
}

/// Approve a pending automation approval.
#[tauri::command]
fn automation_approve(
    db_state: tauri::State<'_, DbState>,
    approval_id: i64,
    user_id: i64,
) -> Result<bool, String> {
    let conn = db_state.lock_conn()?;
    spp_core::automation::approve(&conn, approval_id, user_id).map_err(|e| e.to_string())
}

/// Reject a pending automation approval.
#[tauri::command]
fn automation_reject(
    db_state: tauri::State<'_, DbState>,
    approval_id: i64,
    user_id: i64,
) -> Result<bool, String> {
    let conn = db_state.lock_conn()?;
    spp_core::automation::reject(&conn, approval_id, user_id).map_err(|e| e.to_string())
}

// ─── Sync Health (M2-T05) ─────────────────────────────────────────────────

/// Get the sync health state. Per spec A9: never claim real-time when it's not.
#[tauri::command]
fn sync_health_state(db_state: tauri::State<'_, DbState>) -> Result<serde_json::Value, String> {
    let conn = db_state.lock_conn()?;

    // Check if any sync runs have completed.
    let sync_completed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_runs WHERE status != 'running'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Check if any webhook events have been received.
    let webhook_events: i64 = conn
        .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
        .unwrap_or(0);

    // Determine the webhook push state (A9).
    let (state, is_realtime) = if webhook_events > 0 {
        ("receiving", true)
    } else if sync_completed > 0 {
        ("registered", false)
    } else {
        ("not_configured", false)
    };

    Ok(serde_json::json!({
        "webhook_push_state": state,
        "is_realtime": is_realtime,
        "sync_runs_completed": sync_completed,
        "webhook_events_received": webhook_events,
        "polling_note": "Incremental polling runs every 5 minutes regardless of webhook configuration.",
    }))
}

// ─── Reports (M8-T02) ─────────────────────────────────────────────────────

/// Build a custom report (21 metrics × 14 dimensions).
#[tauri::command]
fn report_build(
    db_state: tauri::State<'_, DbState>,
    metric: String,
    dimension: String,
    days_back: Option<u32>,
) -> Result<serde_json::Value, String> {
    use spp_core::catalog::{ReportDimensionKey, ReportMetricKey};

    let conn = db_state.lock_conn()?;

    // Parse metric + dimension from strings to catalog enums.
    let metric_key = ReportMetricKey::ALL
        .iter()
        .find(|k| k.as_str() == metric)
        .copied()
        .ok_or_else(|| format!("unknown report metric: {metric}"))?;

    let dim_key = ReportDimensionKey::ALL
        .iter()
        .find(|k| k.as_str() == dimension)
        .copied()
        .ok_or_else(|| format!("unknown report dimension: {dimension}"))?;

    let result =
        spp_core::reports::build_report(&conn, metric_key, dim_key, days_back.unwrap_or(30))
            .map_err(|e| e.to_string())?;
    serde_json::to_value(&result).map_err(|e| e.to_string())
}

// ─── Support health (M8-T05) — operational facts, NOT a 0-100 score ─────

/// Get operational health facts (per spec section 58: no aggregate score).
#[tauri::command]
fn support_health(
    db_state: tauri::State<'_, DbState>,
    days_back: Option<u32>,
) -> Result<serde_json::Value, String> {
    let conn = db_state.lock_conn()?;
    let facts = spp_core::reports::get_health_facts(&conn, days_back.unwrap_or(7))
        .map_err(|e| e.to_string())?;
    serde_json::to_value(&facts).map_err(|e| e.to_string())
}

// ─── Intelligence — Issue Radar (M7-T04) ─────────────────────────────────

/// Get the Issue Radar snapshot (active known issues + clusters + incidents).
#[tauri::command]
fn issue_radar_snapshot(db_state: tauri::State<'_, DbState>) -> Result<serde_json::Value, String> {
    let conn = db_state.lock_conn()?;
    let snapshot =
        spp_core::intelligence_features::get_radar_snapshot(&conn).map_err(|e| e.to_string())?;
    serde_json::to_value(&snapshot).map_err(|e| e.to_string())
}

// ─── Conformance (M11-T01) ────────────────────────────────────────────────

/// Run the parity gate — verify all canonical counts match the reference.
#[tauri::command]
fn parity_gate_check() -> Result<serde_json::Value, String> {
    match spp_core::conformance::verify_canonical_counts() {
        Ok(()) => Ok(serde_json::json!({
            "passed": true,
            "total_variants": spp_core::conformance::total_catalog_variants(),
            "message": "All 10 catalog enums match the reference inventory.",
        })),
        Err(e) => Ok(serde_json::json!({
            "passed": false,
            "error": e.to_string(),
        })),
    }
}

// ─── Self-check (M12 — packaging verification) ──────────────────────────

/// Get the startup self-check report. The check runs once at boot; this
/// command returns the cached result. Returns `null` if the self-check
/// failed to run (e.g., the DB connection was unavailable at boot).
#[tauri::command]
fn self_check(db_state: tauri::State<'_, DbState>) -> Result<Option<serde_json::Value>, String> {
    Ok(db_state
        .self_check_report
        .as_ref()
        .and_then(|r| serde_json::to_value(r).ok()))
}

// ─── Inbox (M12-P5) ──────────────────────────────────────────────────────

/// List conversations matching the given filters.
#[tauri::command]
fn inbox_list_conversations(
    db_state: tauri::State<'_, DbState>,
    filters: spp_core::inbox::InboxFilters,
) -> Result<serde_json::Value, String> {
    let conn = db_state.lock_conn()?;
    let (items, total) =
        spp_core::inbox::list_conversations(&conn, &filters).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "items": serde_json::to_value(&items).map_err(|e| e.to_string())?,
        "total": total,
        "filters": serde_json::to_value(&filters).map_err(|e| e.to_string())?,
    }))
}

/// Get a single conversation with its full thread.
#[tauri::command]
fn inbox_get_conversation(
    db_state: tauri::State<'_, DbState>,
    conversation_id: i64,
) -> Result<Option<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let detail =
        spp_core::inbox::get_conversation(&conn, conversation_id).map_err(|e| e.to_string())?;
    match detail {
        Some(d) => Ok(Some(serde_json::to_value(&d).map_err(|e| e.to_string())?)),
        None => Ok(None),
    }
}

/// Reply to a conversation. Creates a reply thread entry + activity event.
#[tauri::command]
fn inbox_reply(
    db_state: tauri::State<'_, DbState>,
    conversation_remote_id: i64,
    body: String,
    actor_type: String,
    actor_id: Option<i64>,
) -> Result<serde_json::Value, String> {
    let mut conn = db_state.lock_conn()?;
    let result = spp_core::inbox::reply_to_conversation(
        &mut conn,
        conversation_remote_id,
        body,
        actor_type,
        actor_id,
    )
    .map_err(|e| e.to_string())?;
    serialize_operation_result(result)
}

/// Add an internal note to a conversation.
#[tauri::command]
fn inbox_add_note(
    db_state: tauri::State<'_, DbState>,
    conversation_remote_id: i64,
    body: String,
    actor_type: String,
    actor_id: Option<i64>,
) -> Result<serde_json::Value, String> {
    let mut conn = db_state.lock_conn()?;
    let result = spp_core::inbox::add_note(
        &mut conn,
        conversation_remote_id,
        body,
        actor_type,
        actor_id,
    )
    .map_err(|e| e.to_string())?;
    serialize_operation_result(result)
}

/// Change the conversation status (active/pending/closed).
#[tauri::command]
fn inbox_change_status(
    db_state: tauri::State<'_, DbState>,
    conversation_remote_id: i64,
    new_status: String,
    actor_type: String,
    actor_id: Option<i64>,
) -> Result<serde_json::Value, String> {
    let mut conn = db_state.lock_conn()?;
    let result = spp_core::inbox::change_status(
        &mut conn,
        conversation_remote_id,
        new_status,
        actor_type,
        actor_id,
    )
    .map_err(|e| e.to_string())?;
    serialize_operation_result(result)
}

/// Assign the conversation to a user (or unassign if assignee_local_id is None).
#[tauri::command]
fn inbox_assign(
    db_state: tauri::State<'_, DbState>,
    conversation_remote_id: i64,
    assignee_local_id: Option<i64>,
    actor_type: String,
    actor_id: Option<i64>,
) -> Result<serde_json::Value, String> {
    let mut conn = db_state.lock_conn()?;
    let result = spp_core::inbox::assign(
        &mut conn,
        conversation_remote_id,
        assignee_local_id,
        actor_type,
        actor_id,
    )
    .map_err(|e| e.to_string())?;
    serialize_operation_result(result)
}

/// List all saved inbox views.
#[tauri::command]
fn inbox_list_saved_views(
    db_state: tauri::State<'_, DbState>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let views = spp_core::inbox::list_saved_views(&conn).map_err(|e| e.to_string())?;
    views
        .into_iter()
        .map(|v| serde_json::to_value(&v).map_err(|e| e.to_string()))
        .collect()
}

/// Apply a saved view — returns the matching conversation IDs.
#[tauri::command]
fn inbox_apply_saved_view(
    db_state: tauri::State<'_, DbState>,
    view_id: i64,
) -> Result<Vec<i64>, String> {
    let conn = db_state.lock_conn()?;
    spp_core::inbox::apply_saved_view(&conn, view_id).map_err(|e| e.to_string())
}

/// Serialize an OperationResult into a JSON value the UI can switch on.
fn serialize_operation_result(
    result: spp_core::ticket_ops::OperationResult,
) -> Result<serde_json::Value, String> {
    match result {
        spp_core::ticket_ops::OperationResult::Success { message } => Ok(serde_json::json!({
            "ok": true,
            "message": message,
        })),
        spp_core::ticket_ops::OperationResult::Rejected { reason } => Ok(serde_json::json!({
            "ok": false,
            "reason": reason,
        })),
    }
}

// ─── Customers (M12-P5b) ────────────────────────────────────────────────

/// Get a customer by local id.
#[tauri::command]
fn customer_get(
    db_state: tauri::State<'_, DbState>,
    customer_id: i64,
) -> Result<Option<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let customer =
        spp_core::customers::get_customer(&conn, customer_id).map_err(|e| e.to_string())?;
    match customer {
        Some(c) => Ok(Some(serde_json::to_value(&c).map_err(|e| e.to_string())?)),
        None => Ok(None),
    }
}

/// List conversations for a customer (most recent first).
#[tauri::command]
fn customer_conversations(
    db_state: tauri::State<'_, DbState>,
    customer_id: i64,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let convs = spp_core::customers::list_customer_conversations(&conn, customer_id, limit)
        .map_err(|e| e.to_string())?;
    convs
        .into_iter()
        .map(|c| serde_json::to_value(&c).map_err(|e| e.to_string()))
        .collect()
}

/// Get the customer timeline — merged view of thread entries across all
/// conversations for this customer.
#[tauri::command]
fn customer_timeline(
    db_state: tauri::State<'_, DbState>,
    customer_id: i64,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let entries = spp_core::customers::customer_timeline(&conn, customer_id, limit)
        .map_err(|e| e.to_string())?;
    entries
        .into_iter()
        .map(|e| serde_json::to_value(&e).map_err(|e| e.to_string()))
        .collect()
}

/// Search customers by name or email.
#[tauri::command]
fn customer_search(
    db_state: tauri::State<'_, DbState>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let customers =
        spp_core::customers::search_customers(&conn, &query, limit).map_err(|e| e.to_string())?;
    customers
        .into_iter()
        .map(|c| serde_json::to_value(&c).map_err(|e| e.to_string()))
        .collect()
}

// ─── Intelligence — Incidents + Knowledge gaps (M12-P5h) ────────────────

/// List incidents (optionally filtered by status string).
/// Status strings: "investigating", "identified", "fix_in_progress",
/// "monitoring", "resolved". None = all statuses.
#[tauri::command]
fn incidents_list(
    db_state: tauri::State<'_, DbState>,
    status: Option<String>,
) -> Result<Vec<serde_json::Value>, String> {
    use spp_core::catalog::IncidentStatus;
    let conn = db_state.lock_conn()?;
    let status_filter = match status.as_deref() {
        Some("investigating") => Some(IncidentStatus::Investigating),
        Some("identified") => Some(IncidentStatus::Identified),
        Some("fix_in_progress") => Some(IncidentStatus::FixInProgress),
        Some("monitoring") => Some(IncidentStatus::Monitoring),
        Some("resolved") => Some(IncidentStatus::Resolved),
        _ => None,
    };
    let incidents = spp_core::intelligence_features::list_incidents(&conn, status_filter)
        .map_err(|e| e.to_string())?;
    incidents
        .into_iter()
        .map(|i| serde_json::to_value(&i).map_err(|e| e.to_string()))
        .collect()
}

/// List knowledge gaps (topics with the most missing docs).
#[tauri::command]
fn knowledge_gaps_list(
    db_state: tauri::State<'_, DbState>,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let gaps = spp_core::intelligence_features::list_knowledge_gaps(&conn, limit.unwrap_or(20))
        .map_err(|e| e.to_string())?;
    gaps.into_iter()
        .map(|(topic, count)| {
            Ok(serde_json::json!({
                "topic": topic,
                "gap_count": count,
            }))
        })
        .collect()
}

// ─── Side threads (M12-P5h) ─────────────────────────────────────────────

/// List side threads for a conversation.
#[tauri::command]
fn side_threads_list(
    db_state: tauri::State<'_, DbState>,
    conversation_id: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let threads =
        spp_core::side_threads::list_side_threads_for_conversation(&conn, conversation_id)
            .map_err(|e| e.to_string())?;
    threads
        .into_iter()
        .map(|t| serde_json::to_value(&t).map_err(|e| e.to_string()))
        .collect()
}

/// List messages in a side thread.
#[tauri::command]
fn side_thread_messages(
    db_state: tauri::State<'_, DbState>,
    thread_id: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let messages = spp_core::side_threads::list_side_thread_messages(&conn, thread_id)
        .map_err(|e| e.to_string())?;
    messages
        .into_iter()
        .map(|m| serde_json::to_value(&m).map_err(|e| e.to_string()))
        .collect()
}

// ─── Connectors (M12-P5h) ──────────────────────────────────────────────

/// List all configured connectors.
#[tauri::command]
fn connectors_list(db_state: tauri::State<'_, DbState>) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let connectors = spp_core::data_tools::list_connectors(&conn).map_err(|e| e.to_string())?;
    connectors
        .into_iter()
        .map(|c| serde_json::to_value(&c).map_err(|e| e.to_string()))
        .collect()
}

// ─── Custom objects (M12-P5h) ──────────────────────────────────────────

/// List all custom object types.
#[tauri::command]
fn custom_object_types_list(
    db_state: tauri::State<'_, DbState>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let types = spp_core::data_tools::list_object_types(&conn).map_err(|e| e.to_string())?;
    types
        .into_iter()
        .map(|t| serde_json::to_value(&t).map_err(|e| e.to_string()))
        .collect()
}

/// List fields for a custom object type.
#[tauri::command]
fn custom_object_fields_list(
    db_state: tauri::State<'_, DbState>,
    type_id: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let fields =
        spp_core::data_tools::list_object_fields(&conn, type_id).map_err(|e| e.to_string())?;
    fields
        .into_iter()
        .map(|f| serde_json::to_value(&f).map_err(|e| e.to_string()))
        .collect()
}

// ─── Outreach (M12-P5i) ───────────────────────────────────────────────

/// List all saved segments.
#[tauri::command]
fn segments_list(db_state: tauri::State<'_, DbState>) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let segments = spp_core::outreach::list_segments(&conn).map_err(|e| e.to_string())?;
    segments
        .into_iter()
        .map(|s| serde_json::to_value(&s).map_err(|e| e.to_string()))
        .collect()
}

/// List all campaigns.
#[tauri::command]
fn campaigns_list(db_state: tauri::State<'_, DbState>) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let campaigns = spp_core::outreach::list_campaigns(&conn).map_err(|e| e.to_string())?;
    campaigns
        .into_iter()
        .map(|c| serde_json::to_value(&c).map_err(|e| e.to_string()))
        .collect()
}

/// List customers on the do-not-contact list.
#[tauri::command]
fn dnc_list(db_state: tauri::State<'_, DbState>) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let dnc = spp_core::outreach::list_dnc(&conn).map_err(|e| e.to_string())?;
    dnc.into_iter()
        .map(|(customer_id, reason, added_at)| {
            Ok(serde_json::json!({
                "customer_id": customer_id,
                "reason": reason,
                "added_at": added_at,
            }))
        })
        .collect()
}

// ─── Search (M12-P5i) ──────────────────────────────────────────────────

/// Universal search across conversations + customers (FTS5).
#[tauri::command]
fn universal_search(
    db_state: tauri::State<'_, DbState>,
    query: String,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    // Ensure FTS5 tables exist (idempotent).
    let _ = spp_core::search::apply_fts_migration(&conn);
    let results = spp_core::search::universal_search(&conn, &query).map_err(|e| e.to_string())?;
    results
        .into_iter()
        .map(|r| serde_json::to_value(&r).map_err(|e| e.to_string()))
        .collect()
}

// ─── Backup (M12-P5i) ──────────────────────────────────────────────────

/// Export all table data as a JSON backup (excluding internal tables).
#[tauri::command]
fn backup_export(db_state: tauri::State<'_, DbState>) -> Result<serde_json::Value, String> {
    let conn = db_state.lock_conn()?;
    let backup = spp_core::data_tools::export_db(&conn).map_err(|e| e.to_string())?;
    serde_json::to_value(&backup).map_err(|e| e.to_string())
}

// ─── Support graph (M12-P5j) ──────────────────────────────────────────

/// List all graph nodes (most recent first).
#[tauri::command]
fn graph_nodes_list(
    db_state: tauri::State<'_, DbState>,
    limit: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let nodes = spp_core::reports::list_graph_nodes(&conn, limit).map_err(|e| e.to_string())?;
    nodes
        .into_iter()
        .map(|n| serde_json::to_value(&n).map_err(|e| e.to_string()))
        .collect()
}

/// Get neighbors of a graph node.
#[tauri::command]
fn graph_neighbors(
    db_state: tauri::State<'_, DbState>,
    node_id: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db_state.lock_conn()?;
    let neighbors =
        spp_core::reports::get_graph_neighbors(&conn, node_id).map_err(|e| e.to_string())?;
    neighbors
        .into_iter()
        .map(|n| serde_json::to_value(&n).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_returns_pong() {
        assert_eq!(ping(), "pong");
    }

    #[test]
    fn version_is_set() {
        assert!(!version().is_empty());
    }

    #[test]
    fn catalog_counts_match_spec() {
        let c = catalog_counts();
        assert_eq!(c["activity_fields"], 14);
        assert_eq!(c["date_modes"], 15);
        assert_eq!(c["condition_kinds"], 22);
        assert_eq!(c["operations_tiles"], 16);
        assert_eq!(c["notification_types"], 15);
        assert_eq!(c["report_metrics"], 21);
        assert_eq!(c["report_dimensions"], 14);
        assert_eq!(c["graph_node_kinds"], 12);
        assert_eq!(c["ai_attribute_keys"], 14);
        assert_eq!(c["copilot_tools"], 22);
    }

    #[test]
    fn copilot_allowlist_has_22_tools() {
        let list = copilot_allowlist();
        assert_eq!(list.len(), 22);
    }

    #[test]
    fn parity_gate_passes() {
        let result = parity_gate_check().unwrap();
        assert_eq!(result["passed"], true);
        assert_eq!(result["total_variants"], 165);
    }

    #[test]
    fn first_run_state_reads_false_on_fresh_db() {
        use tempfile::NamedTempFile;
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = spp_core::db::open(&f).unwrap();
        spp_core::db::ensure_migrations_table(&conn).unwrap();
        spp_core::migrations::run_all(&mut conn).unwrap();

        let done = spp_core::settings::first_run_done(&conn).unwrap();
        assert!(!done, "first_run_done must be false on fresh DB");
    }

    #[test]
    fn first_run_state_marks_done_on_write() {
        use tempfile::NamedTempFile;
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = spp_core::db::open(&f).unwrap();
        spp_core::db::ensure_migrations_table(&conn).unwrap();
        spp_core::migrations::run_all(&mut conn).unwrap();

        assert!(!spp_core::settings::first_run_done(&conn).unwrap());
        spp_core::settings::mark_first_run_done(&conn).unwrap();
        assert!(spp_core::settings::first_run_done(&conn).unwrap());
    }

    #[test]
    fn open_db_with_all_migrations_applies_m003_through_m027() {
        use tempfile::NamedTempFile;
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let conn = open_db_with_all_migrations(&f).unwrap();

        // Verify M003 tables exist.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
            .unwrap();
        // Verify M005 tables exist.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        // Verify M007 tables exist.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM automation_rules", [], |r| r.get(0))
            .unwrap();
        // Verify M014 tables exist.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM interaction_signals", [], |r| r.get(0))
            .unwrap();
        // Verify M020 tables exist.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM friction_scores", [], |r| r.get(0))
            .unwrap();
        // Verify M025 tables exist.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM do_not_contact", [], |r| r.get(0))
            .unwrap();
        // Verify M027 tables exist.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM connectors", [], |r| r.get(0))
            .unwrap();
        // Verify M028 tables exist.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_threads", [], |r| {
                r.get(0)
            })
            .unwrap();
    }

    #[test]
    fn self_check_report_is_honest_on_fresh_db() {
        use tempfile::NamedTempFile;
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let conn = open_db_with_all_migrations(&f).unwrap();
        // Pass a mock loopback addr so the loopback_listener check passes.
        // (In production, the actual bind may fail and the self-check will
        // honestly report `ok: false` for that subsystem.)
        let report = spp_core::self_check::run(&conn, Some("127.0.0.1:0".to_string())).unwrap();

        // The self-check must report all subsystems ok on a fresh DB
        // (when the loopback listener is bound).
        assert!(
            report.all_ok,
            "self-check failed: {:?}",
            report
                .subsystems
                .iter()
                .filter(|s| !s.ok)
                .collect::<Vec<_>>()
        );
        // The Qdrant feature is OFF by default; the report must say so.
        assert!(
            !report.qdrant_feature_enabled,
            "qdrant feature must be off by default"
        );
        let vs = report
            .subsystems
            .iter()
            .find(|s| s.name == "vector_store")
            .unwrap();
        assert!(vs.status.contains("in_memory"));
        assert!(vs.status.contains("NOT enabled"));
        // The database check must confirm all 27 migrations applied.
        let db_check = report
            .subsystems
            .iter()
            .find(|s| s.name == "database")
            .unwrap();
        assert!(db_check.ok, "db_check failed: {:?}", db_check);
        let details = db_check.details.as_ref().unwrap();
        assert_eq!(details["schema_version"], 28);
    }
}
