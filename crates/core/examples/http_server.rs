//! Standalone HTTP server — for differential testing without the Tauri shell.
//!
//! Boots JUST the axum HTTP server on 127.0.0.1:3001 (configurable via
//! `SPP_HTTP_PORT`). Uses a fresh SQLite DB with all migrations applied.
//! Demo mode is forced on so the demo endpoints work.
//!
//! Usage:
//!     cargo run -p supportos-plusplus-core --example http_server
//!     SPP_HTTP_PORT=4000 cargo run -p supportos-plusplus-core --example http_server
//!
//! This binary is for development / testing only. The real production
//! binary is the Tauri shell (`crates/app/src-tauri`), which boots the
//! HTTP server alongside the Tauri IPC commands.

use std::sync::{Arc, Mutex};

use spp_core::config::AppConfig;
use spp_core::http::server::AppState;
use spp_core::http::{EventBus, HttpServer, RateLimiter};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    spp_core::logging::init();
    tracing::info!("SupportOS++ standalone HTTP server starting");

    let port: u16 = std::env::var("SPP_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3001);

    // Open SQLite with all migrations applied.
    let app_config = AppConfig::default();
    let db_path = app_config.data_dir.join("supportos-plusplus.db");
    let mut conn = match spp_core::db::open_with_migrations(&db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to open DB; falling back to in-memory");
            let mut in_mem = rusqlite::Connection::open_in_memory().expect("in-memory fallback");
            let _ = spp_core::db::ensure_migrations_table(&in_mem);
            let _ = spp_core::migrations::run_all(&mut in_mem);
            in_mem
        }
    };
    let _ = spp_core::db::ensure_migrations_table(&conn);
    let _ = spp_core::migrations::run_all(&mut conn);

    // Apply per-module migrations (these create the tables the HTTP routes query).
    let _ = spp_core::search::apply_fts_migration(&conn);
    let _ = spp_core::activity::apply_m003(&conn);
    let _ = spp_core::ticket_states::apply_m004(&conn);
    let _ = spp_core::notifications::apply_m005(&conn);
    let _ = spp_core::side_threads::apply_m006(&conn);
    let _ = spp_core::automation::apply_m007(&conn);
    let _ = spp_core::embeddings::apply_m008(&conn);
    let _ = spp_core::ai_center::apply_m009(&conn);
    let _ = spp_core::ai_analysis::apply_m010(&conn);
    let _ = spp_core::ai_features::apply_m011_to_m013(&conn);
    let _ = spp_core::intelligence::apply_m014(&conn);
    let _ = spp_core::intelligence_features::apply_m015_to_m019(&conn);
    let _ = spp_core::reports::apply_m020_to_m022(&conn);
    let _ = spp_core::outreach::apply_m023_to_m025(&conn);
    let _ = spp_core::data_tools::apply_m026_to_m027(&conn);
    let _ = spp_core::inbox::apply_m028(&conn);
    let _ = spp_core::sync_schema::apply_m029(&conn);
    let _ = spp_core::webhook::ensure_webhook_events_table(&conn);
    let _ = spp_core::saved_views::ensure_saved_views_table(&conn);
    let _ = spp_core::oauth_state::ensure_oauth_states_table(&conn);
    let _ = spp_core::jobs::ensure_jobs_table(&conn);
    // Recover jobs stuck in running (reference recoverStaleJobs at boot).
    let _ = spp_core::jobs::recover_stale_jobs(&conn);

    // Mark first run done + enable demo mode (so demo endpoints work).
    let _ = spp_core::settings::mark_first_run_done(&conn);
    let _ = spp_core::settings::set_bool(&conn, "demo_mode", true);

    let demo_mode = spp_core::settings::get_bool(&conn, "demo_mode", false).unwrap_or(true);

    // Provider selection (reference context.ts): demo mode -> Fake, else Real
    // with env credentials. SPP_FORCE_REAL=1 keeps the real provider even in
    // demo-flagged DBs (differential testing against a live account).
    let force_real = std::env::var("SPP_FORCE_REAL")
        .map(|v| v == "1")
        .unwrap_or(false);
    let credentials = spp_core::helpscout_real::HsCredentials::from_env();
    let use_real = (!demo_mode || force_real) && credentials.is_configured();

    let conn = Arc::new(Mutex::new(conn));
    let bus = EventBus::default();

    let (provider, real, provider_kind): (
        Arc<dyn spp_core::helpscout::HelpScoutProvider>,
        Option<Arc<spp_core::helpscout_real::RealHelpScoutProvider>>,
        String,
    ) = if use_real {
        let real = Arc::new(spp_core::helpscout_real::RealHelpScoutProvider::new(
            conn.clone(),
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
        spp_core::sync_engine::SyncEngine::new(conn.clone(), provider).with_bus(bus.clone()),
    );

    let state = AppState {
        conn,
        data_dir: app_config.data_dir.clone(),
        port,
        host: "127.0.0.1".to_string(),
        demo_mode,
        bus,
        limiter: RateLimiter::new(),
        sync: Some(sync),
        real,
        provider_kind,
    };

    let server = HttpServer::new(state);
    let addr = server.addr();
    tracing::info!(%addr, "HTTP API server bound — differential test mode");
    println!("HTTP API server listening on {addr}");

    server.serve().await
}
