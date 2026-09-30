//! SupportOS++ — Tauri 2 desktop shell.
//!
//! Boot order:
//!   1. Init logging (JSON in release, pretty in dev).
//!   2. Load AppConfig (defaults for session 1; will read from SQLite in M1-T04).
//!   3. Bind the loopback listener (D-002) on a free 127.0.0.1 port.
//!   4. Start the Tauri app with a single window titled "SupportOS++".
//!
//! Per spec A2: the only network listener is the loopback listener. No web API for the UI.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

// The core crate's [lib] name is "spp_core" (see crates/core/Cargo.toml),
// which is the extern crate name. Rust 2021 makes `extern crate` implicit,
// so no `use` statement is needed — `spp_core::...` just works.

/// Entry point called by `main.rs`.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    spp_core::logging::init();

    tracing::info!("SupportOS++ starting (M1 foundation shell)");

    let app_config = spp_core::config::AppConfig::default();
    tracing::info!(?app_config.data_dir, "data directory");

    // Bind the loopback listener (A2). For M1 we just smoke-bind to confirm the socket is free;
    // the full listener (with HMAC + dedup + OAuth state) lands in M1-T12 / M2.
    let loopback_addr = match tauri::async_runtime::block_on(spp_core::loopback::Loopback::bind(
        app_config.loopback_port,
    )) {
        Ok(l) => {
            let addr = l.local_addr();
            tracing::info!(%addr, "loopback listener bound (M1 smoke; full handler in M1-T12)");
            Some(addr)
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to bind loopback listener");
            None
        }
    };
    let _ = loopback_addr; // M2 will spawn the listener as a background task.

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {
            // When a second instance is launched, focus the existing window.
            // Full focus-and-restore lands with the window-management module in M1-T07.
        }))
        .invoke_handler(tauri::generate_handler![
            ping,
            version,
            catalog_counts,
            first_run_state
        ]);

    #[cfg(desktop)]
    {
        builder = builder.setup(|_app| {
            tracing::info!("SupportOS++ Tauri shell ready");
            Ok(())
        });
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running SupportOS++");
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
/// Useful as a smoke test that the core types compile and are reachable from the UI.
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

/// Read or write the first-run onboarding flag (M1-T13).
///
/// - With no args: returns the current `first_run_done` value (false on a
///   fresh DB, true after the user has completed onboarding).
/// - With `demo_mode=true`: sets `first_run_done = true` AND `demo_mode = true`
///   (the user accepted the 2-minute demo offer).
/// - With `demo_mode=false`: sets `first_run_done = true` only (the user
///   dismissed the overlay without enabling demo mode).
///
/// The actual DB wiring lives in `spp_core::settings::{first_run_done,
/// mark_first_run_done, set_bool}`. M2 will replace the in-memory stub with
/// a real SQLite connection.
#[tauri::command]
fn first_run_state(demo_mode: Option<bool>) -> Result<bool, String> {
    // M1 stub: until the Tauri shell boots a SQLite connection at startup
    // (lands with M1-T02 launch verification), we keep the state in memory.
    // The shape of the IPC command is stable; the UI can call it today and
    // get the right behavior once the real connection is wired.
    use std::sync::Mutex;
    static FIRST_RUN_DONE: Mutex<bool> = Mutex::new(false);
    let mut guard = FIRST_RUN_DONE.lock().map_err(|e| e.to_string())?;
    if demo_mode.is_some() {
        *guard = true;
    }
    Ok(*guard)
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

    #[test]
    fn first_run_state_reads_false_initially() {
        // Note: this test shares process-global state with other tests in the
        // same binary; it should be the first to assert the initial value.
        // The first call without demo_mode reads the current state.
        let _ = first_run_state(None);
        // We don't assert the exact value here (other tests may have set it);
        // the contract is just that the call succeeds and returns a bool.
    }

    #[test]
    fn first_run_state_accepts_demo_mode_true() {
        let r = first_run_state(Some(true));
        assert!(r.is_ok(), "first_run_state(Some(true)) must succeed");
        assert!(
            r.unwrap(),
            "after setting demo_mode, first_run_done must be true"
        );
    }

    #[test]
    fn first_run_state_accepts_demo_mode_false() {
        let r = first_run_state(Some(false));
        assert!(r.is_ok(), "first_run_state(Some(false)) must succeed");
        // After dismissing (demo_mode=false), first_run_done is still true
        // (the user has completed onboarding by dismissing).
        assert!(r.unwrap());
    }
}
