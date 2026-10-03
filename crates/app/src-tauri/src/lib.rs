//! SupportOS++ — Tauri 2 desktop shell.
//!
//! The core application is the local HTTP server in `crates/core` (the Rust
//! counterpart of the reference's Fastify backend); this shell boots that
//! server on 127.0.0.1 and opens a window on the bundled Leptos UI, exactly
//! like the reference shell launches its Node backend and opens a webview.
//! No IPC commands exist — the UI is a pure HTTP/SSE client, like the
//! reference's React client.
//!
//! Boot order:
//!   1. Init logging (JSON in release, pretty in dev).
//!   2. Load AppConfig (defaults; data_dir from SPP_DATA_DIR or per-OS convention).
//!   3. Open SQLite with all migrations applied (single connection, owned by
//!      the HTTP server).
//!   4. Bind the loopback listener (OAuth callback + webhook receiver).
//!   5. Start the HTTP API server on 127.0.0.1:3000.
//!   6. Run the Tauri app (window comes from tauri.conf.json).

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use std::sync::{Arc, Mutex};

use spp_core::config::AppConfig;

/// Entry point called by `main.rs`.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    spp_core::logging::init();

    tracing::info!("SupportOS++ starting");

    // 1. Load the config (data_dir from SPP_DATA_DIR or per-OS convention).
    let app_config = AppConfig::default();
    tracing::info!(?app_config.data_dir, "data directory");

    // 2. Open the SQLite DB with migrations. The connection is owned by the
    //    HTTP server (single owner, like the reference backend's db handle).
    //    If it cannot be opened the app cannot run: surface the failure and
    //    exit (reference shell behavior on backend failure).
    let db_path = app_config.data_dir.join("supportos-plusplus.db");
    let conn = match open_db_with_all_migrations(&db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, path = %db_path.display(), "failed to open SQLite DB");
            eprintln!("[supportos++] backend error: failed to open SQLite DB: {e}");
            std::process::exit(1);
        }
    };

    // 3. Bind the loopback listener — for OAuth callback + webhook receiver.
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
    let _loopback_addr_str = loopback_addr.map(|a| a.to_string());

    // 4. Start the HTTP API server on 127.0.0.1:3000 (mirrors the reference's
    //    Fastify server; serves the webview AND any browser client on
    //    localhost — webhooks, SSE events, and demo endpoints included).
    let http_port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let demo_mode = spp_core::settings::get_bool(&conn, "demo_mode", false).unwrap_or(false)
        || std::env::var("LOCAL_DEMO_MODE").as_deref() == Ok("true");

    let http_conn = Arc::new(Mutex::new(conn));
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

    // 5. Run the Tauri app. The window (title/size/frontendDist) comes from
    //    tauri.conf.json; the UI talks to the HTTP API like the reference's
    //    web client.
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Second launch: focus the existing window instead of spawning
            // another backend (reference shell behavior).
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
        .run(tauri::generate_context!())
        .expect("error while running SupportOS++");
}

/// Open the SQLite DB and apply the full migration chain (verbatim from the
/// pre-thin-shell app bootstrap).
fn open_db_with_all_migrations(
    path: &std::path::Path,
) -> spp_core::error::Result<rusqlite::Connection> {
    let mut conn = spp_core::db::open(path)?;
    spp_core::db::ensure_migrations_table(&conn)?;
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

    tracing::info!("All migrations applied successfully");
    Ok(conn)
}
