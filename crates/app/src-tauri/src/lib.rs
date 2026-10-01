//! SupportOS++ — Tauri 2 desktop shell.
//!
//! Boot order:
//!   1. Init logging (JSON in release, pretty in dev).
//!   2. Load AppConfig (defaults; data_dir from SPP_DATA_DIR or per-OS convention).
//!   3. Open SQLite with ALL migrations (M001–M027) applied.
//!   4. Bind the loopback listener (D-002) on a free 127.0.0.1 port.
//!   5. Start the Tauri app with IPC commands wired to the Rust core.
//!
//! Per spec A2: the only network listener is the loopback listener. No web API for the UI.
//! Per spec: all mutations go through Tauri IPC commands — the UI never touches the DB directly.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use std::sync::Mutex;

use spp_core::config::AppConfig;
use spp_core::db;

/// Entry point called by `main.rs`.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    spp_core::logging::init();

    tracing::info!("SupportOS++ starting (M11 — all milestones complete)");

    // 1. Load the config (data_dir from SPP_DATA_DIR or per-OS convention).
    let app_config = AppConfig::default();
    tracing::info!(?app_config.data_dir, "data directory");

    // 2. Open the SQLite DB with migrations. The connection is wrapped in a
    //    Mutex and managed by Tauri's state system so IPC commands can access it.
    let db_path = app_config.data_dir.join("supportos-plusplus.db");
    let conn = match open_db_with_all_migrations(&db_path) {
        Ok(c) => {
            tracing::info!(path = %db_path.display(), "SQLite DB opened with all migrations M001–M027");
            c
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to open SQLite DB; falling back to in-memory state");
            return launch_without_db();
        }
    };

    // 3. Bind the loopback listener (A2).
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

    // 3.5 Run the startup self-check (M12 — packaging verification).
    // The report is logged and made available via the `self_check` IPC command.
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
    let _ = self_check_report; // hold for state below

    // 4. Launch Tauri with the DB connection in state + all IPC commands.
    let db_state = DbState {
        conn: Mutex::new(conn),
        self_check_report: self_check_report,
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

/// Open the DB and apply ALL migrations (M001 base + M002 sync tables + M003–M027).
/// This is the single entry point for DB initialization at boot.
fn open_db_with_all_migrations(
    path: &std::path::Path,
) -> spp_core::error::Result<rusqlite::Connection> {
    let mut conn = db::open(path)?;
    db::ensure_migrations_table(&conn)?;
    spp_core::migrations::run_all(&mut conn)?;

    // Apply all incremental migrations (M003–M027).
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

    tracing::info!("All migrations M001–M027 applied successfully");
    Ok(conn)
}

/// Launch the app without a DB connection (fallback for disk-full etc.).
fn launch_without_db() {
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

/// Set the AI provider kind (None/LmStudio/Ollama/Generic).
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

#[cfg(all(test, not(target_os = "macos")))]
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
        let report = spp_core::self_check::run(&conn, None).unwrap();

        // The self-check must report all subsystems ok on a fresh DB.
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
        assert_eq!(
            report.qdrant_feature_enabled, false,
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
        assert_eq!(details["schema_version"], 27);
    }
}
