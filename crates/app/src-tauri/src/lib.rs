//! SupportOS++ — Tauri 2 desktop shell.
//!
//! Boot order:
//!   1. Init logging (JSON in release, pretty in dev).
//!   2. Load AppConfig (defaults; data_dir from SPP_DATA_DIR or per-OS convention).
//!   3. Open SQLite with migrations (the DB connection is shared with IPC commands).
//!   4. Bind the loopback listener (D-002) on a free 127.0.0.1 port.
//!   5. Start the Tauri app with a single window titled "SupportOS++".
//!
//! Per spec A2: the only network listener is the loopback listener. No web API for the UI.

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

    tracing::info!("SupportOS++ starting (M2 — Help Scout mirror)");

    // 1. Load the config (data_dir from SPP_DATA_DIR or per-OS convention).
    let app_config = AppConfig::default();
    tracing::info!(?app_config.data_dir, "data directory");

    // 2. Open the SQLite DB with migrations. The connection is wrapped in a
    //    Mutex and managed by Tauri's state system so IPC commands can access it.
    let db_path = app_config.data_dir.join("supportos-plusplus.db");
    let conn = match db::open_with_migrations(&db_path) {
        Ok(c) => {
            tracing::info!(path = %db_path.display(), "SQLite DB opened with migrations");
            c
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to open SQLite DB; falling back to in-memory state");
            // If the DB can't be opened (e.g. disk full), we still let the app
            // launch so the user sees an error state, not a crash. The IPC
            // commands will return errors for DB operations.
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
    let _ = loopback_addr; // M2-T05 will spawn the listener as a background task.

    // 4. Launch Tauri with the DB connection in state.
    let db_state = DbState {
        conn: Mutex::new(conn),
    };

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {
            // When a second instance is launched, focus the existing window.
        }))
        .manage(db_state)
        .invoke_handler(tauri::generate_handler![
            ping,
            version,
            catalog_counts,
            first_run_state,
        ]);

    #[cfg(desktop)]
    {
        builder = builder.setup(|_app| {
            tracing::info!("SupportOS++ Tauri shell ready (M2)");
            Ok(())
        });
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running SupportOS++");
}

/// Launch the app without a DB connection (fallback for disk-full etc.).
fn launch_without_db() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .invoke_handler(tauri::generate_handler![ping, version, catalog_counts,]);

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

/// The shared DB state managed by Tauri. IPC commands access this via
/// `tauri::State<DbState>`.
pub struct DbState {
    /// The SQLite connection. Wrapped in a Mutex for thread safety (Tauri
    /// IPC commands run on a thread pool).
    pub conn: Mutex<rusqlite::Connection>,
}

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
        "ai_attribute_keys": AiAttributeKey::ALL.len()
    })
}

/// Read or write the first-run onboarding flag (M2-T07 — wired to real DB).
///
/// - With no args: returns the current `first_run_done` value from the DB.
/// - With `demo_mode=true`: sets `first_run_done = true` AND `demo_mode = true`
///   in the DB (the user accepted the 2-minute demo offer).
/// - With `demo_mode=false`: sets `first_run_done = true` only (the user
///   dismissed the overlay without enabling demo mode).
#[tauri::command]
fn first_run_state(
    db_state: tauri::State<'_, DbState>,
    demo_mode: Option<bool>,
) -> Result<bool, String> {
    let conn = db_state.conn.lock().map_err(|e| e.to_string())?;

    if let Some(demo) = demo_mode {
        // Write: mark first-run done + optionally enable demo mode.
        spp_core::settings::mark_first_run_done(&conn).map_err(|e| e.to_string())?;
        if demo {
            spp_core::settings::set_bool(&conn, "demo_mode", true).map_err(|e| e.to_string())?;
        }
    }

    // Read: return the current flag.
    let done = spp_core::settings::first_run_done(&conn).map_err(|e| e.to_string())?;
    Ok(done)
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
    }

    /// Test the first_run_state logic with a real DB (no Tauri State wrapper).
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

        // Before: false.
        assert!(!spp_core::settings::first_run_done(&conn).unwrap());

        // Mark done.
        spp_core::settings::mark_first_run_done(&conn).unwrap();

        // After: true.
        assert!(spp_core::settings::first_run_done(&conn).unwrap());
    }

    #[test]
    fn first_run_state_sets_demo_mode() {
        use tempfile::NamedTempFile;
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = spp_core::db::open(&f).unwrap();
        spp_core::db::ensure_migrations_table(&conn).unwrap();
        spp_core::migrations::run_all(&mut conn).unwrap();

        // Before: demo_mode is false (default).
        assert!(!spp_core::settings::get_bool(&conn, "demo_mode", false).unwrap());

        // Mark done + enable demo mode.
        spp_core::settings::mark_first_run_done(&conn).unwrap();
        spp_core::settings::set_bool(&conn, "demo_mode", true).unwrap();

        // After: both true.
        assert!(spp_core::settings::first_run_done(&conn).unwrap());
        assert!(spp_core::settings::get_bool(&conn, "demo_mode", false).unwrap());
    }
}
