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

use supportos_plusplus_core as spp_core;

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
        .invoke_handler(tauri::generate_handler![ping, version, catalog_counts]);

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
}
