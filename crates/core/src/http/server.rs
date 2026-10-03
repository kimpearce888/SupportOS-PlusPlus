//! HTTP server — axum server that mirrors the reference's Fastify HTTP API.
//!
//! Binds to 127.0.0.1:3000 (configurable). Serves all API routes, SSE events,
//! webhooks, and demo endpoints. The Tauri shell can run this alongside its
//! IPC commands so both the browser client and the desktop app work.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Router,
};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::error::Result;

use super::rate_limit::RateLimiter;
use super::routes;
use super::EventBus;
use std::sync::{Mutex, MutexGuard};

/// Helper to lock the AppState mutex. Recovers from a poisoned mutex
/// (which happens when a previous handler panicked while holding the
/// lock) by clearing the poison and continuing — the SQLite connection
/// is still valid after a panic in most cases. This keeps the HTTP
/// server usable even if one route handler panics on an unexpected
/// SQL error.
pub fn lock_or_recover<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| {
        // Clear the poison flag by force-recovering — the underlying
        // data is still there, just flagged as poisoned because a
        // thread panicked while holding the lock.
        tracing::warn!("AppState mutex was poisoned — recovering");
        poisoned.into_inner()
    })
}

/// Shared application state for all HTTP handlers.
/// This wraps the SQLite connection + config + event bus + rate limiter,
/// similar to the reference's AppContext + EventEmitter + mutationHits map.
#[derive(Clone)]
pub struct AppState {
    /// Shared SQLite connection (rusqlite is not Sync alone, so we wrap in Mutex).
    pub conn: Arc<std::sync::Mutex<rusqlite::Connection>>,
    /// Application data directory (where the SQLite DB lives).
    pub data_dir: std::path::PathBuf,
    /// Port the HTTP server binds to.
    pub port: u16,
    /// Host the HTTP server binds to (always 127.0.0.1).
    pub host: String,
    /// Whether demo mode is enabled (mock Help Scout data).
    pub demo_mode: bool,
    /// Real-time event bus — pushes LiveEvents to all connected SSE clients.
    pub bus: EventBus,
    /// Mutation rate limiter — 300 writes/min per IP, mirrors the reference.
    pub limiter: RateLimiter,
    /// The sync coordinator (reference SyncCoordinator). None in unit tests.
    pub sync: Option<Arc<crate::sync_engine::SyncEngine>>,
    /// The real Help Scout provider (reference ctx.realProvider). None in
    /// demo mode / unit tests.
    pub real: Option<Arc<crate::helpscout_real::RealHelpScoutProvider>>,
    /// "fake" or "real" — which provider backs the app.
    pub provider_kind: String,
    /// The background WorkerManager (8 timers), when the server owns it.
    /// `None` in unit tests and before `serve()` starts the workers.
    pub workers: Option<Arc<crate::workers::WorkerManager>>,
}

impl AppState {
    /// Lock the SQLite connection, recovering from a poisoned mutex
    /// instead of panicking. This is the safe way to get the conn
    /// from route handlers — use `state.conn_lock()` instead of
    /// `state.conn.lock().expect("mutex poisoned")`.
    pub fn conn_lock(&self) -> MutexGuard<'_, rusqlite::Connection> {
        lock_or_recover(&self.conn)
    }
}

/// The HTTP server. Owns the bound socket address.
pub struct HttpServer {
    addr: SocketAddr,
    state: AppState,
}

impl HttpServer {
    /// Create a new HTTP server with the given state.
    pub fn new(state: AppState) -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], state.port)),
            state,
        }
    }

    /// The address the server will bind to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Build the axum Router with all routes.
    pub fn build_router(&self) -> Router {
        let state = self.state.clone();
        let limiter = self.state.limiter.clone();

        // DNS-rebinding guard: reject requests with non-loopback Host headers.
        // This matches the reference's isLoopbackHostHeader check.
        let dns_guard = axum::middleware::from_fn(
            |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let host = req
                    .headers()
                    .get(axum::http::header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");
                if !is_loopback_host(host) {
                    return axum::response::Response::builder()
                    .status(axum::http::StatusCode::FORBIDDEN)
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"statusCode":403,"error":"Forbidden","message":"SupportOS is a local application: requests with non-loopback Host headers are refused (DNS-rebinding guard)."}"#,
                    ))
                    .unwrap();
                }
                next.run(req).await
            },
        );

        // Mutation rate limiter — mirrors the reference's `mutationHits` map:
        //   - 300 mutations / 60s / client IP
        //   - GET / HEAD / OPTIONS unmetered
        //   - `/api/webhooks/helpscout` exempt (HMAC-authenticated + deduped)
        //   - Keyed on socket remoteAddress (NOT X-Forwarded-For, which is spoofable)
        //   - 429 with `retry-after` header + JSON error body on overflow.
        let rate_limit = axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let limiter = limiter.clone();
                async move {
                    let method = req.method().clone();
                    let path = req.uri().path().to_string();
                    // Key on the socket address (constant for localhost users).
                    // The ConnectInfo extractor gives us this for free.
                    let key = req
                        .extensions()
                        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                        .map(|ci| ci.ip().to_string())
                        .unwrap_or_else(|| "local".to_string());
                    if let Err(retry_after) = limiter.check(&method, &path, &key) {
                        return RateLimiter::too_many_requests_response(retry_after);
                    }
                    next.run(req).await
                }
            },
        );

        // CORS: localhost-only (matching the reference's allowedOrigins
        // exactly: configured port for localhost/127.0.0.1/[::1], plus the
        // Vite dev ports 5173-5175 for both loopback names).
        let cors = CorsLayer::new()
            .allow_origin([
                format!("http://localhost:{}", self.state.port)
                    .parse()
                    .unwrap(),
                format!("http://127.0.0.1:{}", self.state.port)
                    .parse()
                    .unwrap(),
                format!("http://[::1]:{}", self.state.port).parse().unwrap(),
                "http://localhost:5173".parse().unwrap(),
                "http://localhost:5174".parse().unwrap(),
                "http://localhost:5175".parse().unwrap(),
                "http://127.0.0.1:5173".parse().unwrap(),
                "http://127.0.0.1:5174".parse().unwrap(),
                "http://127.0.0.1:5175".parse().unwrap(),
            ])
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PUT,
                axum::http::Method::PATCH,
                axum::http::Method::DELETE,
                axum::http::Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::AUTHORIZATION,
            ]);

        Router::new()
            // Health
            .route("/health", get(routes::system::health))
            .route("/health/detailed", get(routes::system::health_detailed))
            // System
            .route("/api/system/db", get(routes::system::db_stats))
            .route(
                "/api/system/capabilities",
                get(routes::system::capabilities),
            )
            .route("/api/system/tables", get(routes::system::table_stats))
            // Onboarding
            .route("/api/onboarding", get(routes::system::onboarding))
            .route(
                "/api/onboarding/step",
                post(routes::system::onboarding_step),
            )
            .route(
                "/api/onboarding/complete",
                post(routes::system::onboarding_complete),
            )
            // Demo
            .route("/api/demo/enable", post(routes::system::demo_enable))
            .route(
                "/api/demo/simulate-incoming",
                post(routes::system::demo_simulate_incoming),
            )
            .route(
                "/api/demo/simulate-rating",
                post(routes::system::demo_simulate_rating),
            )
            .route(
                "/api/demo/simulate-webhook",
                post(routes::system::demo_simulate_webhook),
            )
            // SSE events
            .route("/api/events", get(routes::events::sse_handler))
            // Conversations
            .route("/api/conversations", get(routes::conversations::list))
            .route("/api/conversations/:id", get(routes::conversations::get))
            .route(
                "/api/conversations/:id/reply",
                post(routes::conversations::reply),
            )
            .route(
                "/api/conversations/:id/note",
                post(routes::conversations::note),
            )
            .route(
                "/api/conversations/:id/status",
                post(routes::conversations::status),
            )
            .route(
                "/api/conversations/:id/assign",
                post(routes::conversations::assign),
            )
            .route(
                "/api/conversations/:id/priority",
                post(routes::conversations::priority),
            )
            .route(
                "/api/conversations/:id/subject",
                post(routes::conversations::subject),
            )
            .route(
                "/api/conversations/:id/state",
                post(routes::conversations::set_state),
            )
            // Conversation write operations (reference conversations.ts:385-477)
            .route(
                "/api/conversations/:id/move",
                post(routes::conversations::move_to_inbox),
            )
            .route(
                "/api/conversations/:id/tags",
                post(routes::conversations::update_tags_route),
            )
            .route(
                "/api/conversations/:id/fields",
                post(routes::conversations::update_fields_route),
            )
            .route(
                "/api/conversations/:id/snooze",
                post(routes::conversations::snooze_route)
                    .delete(routes::conversations::unsnooze_route),
            )
            .route(
                "/api/conversations/:id/schedule",
                post(routes::conversations::schedule_route)
                    .delete(routes::conversations::schedule_delete_route),
            )
            .route(
                "/api/conversations/:id/schedule/publish",
                post(routes::conversations::schedule_publish_route),
            )
            .route(
                "/api/conversations/bulk",
                post(routes::conversations::bulk_route),
            )
            .route(
                "/api/conversations/:id/refresh",
                post(routes::conversations::refresh_route),
            )
            .route(
                "/api/conversations/:id/workflow/:workflowId",
                post(routes::conversations::workflow_route),
            )
            .route(
                "/api/attachments/:id/download",
                post(routes::conversations::attachment_download_route),
            )
            .route(
                "/api/attachments/:id/file",
                get(routes::system::attachment_file),
            )
            .route(
                "/api/conversations/:id/events",
                get(routes::conversations::events),
            )
            .route(
                "/api/conversations/activity/rebuild",
                post(routes::conversations::activity_rebuild),
            )
            .route(
                "/api/ticket-states",
                get(routes::conversations::list_ticket_states)
                    .post(routes::conversations::create_ticket_state),
            )
            .route(
                "/api/ticket-states/:id",
                axum::routing::patch(routes::conversations::update_ticket_state)
                    .delete(routes::conversations::delete_ticket_state),
            )
            // Reference data endpoints — used by the React/Leptos UI to
            // populate dropdowns, typeahead, etc. Mirrors the reference's
            // routes/conversations.ts reference-data block.
            .route("/api/mailboxes", get(routes::conversations::list_mailboxes))
            .route("/api/tags", get(routes::conversations::list_tags))
            .route("/api/users", get(routes::conversations::list_users))
            .route("/api/teams", get(routes::conversations::list_teams))
            .route(
                "/api/saved-replies",
                get(routes::conversations::list_saved_replies),
            )
            .route(
                "/api/inbox-fields",
                get(routes::conversations::inbox_fields),
            )
            .route("/api/workflows", get(routes::conversations::workflows))
            .route(
                "/api/users/statuses",
                get(routes::conversations::user_statuses),
            )
            .route(
                "/api/webhook-configs",
                get(routes::conversations::webhook_configs),
            )
            // People (customers + organizations)
            .route("/api/customers", get(routes::people::list_customers))
            .route("/api/customers/:id", get(routes::people::get_customer))
            .route(
                "/api/customers/:id/timeline",
                get(routes::people::customer_timeline),
            )
            .route(
                "/api/customers/:id/support-health",
                get(routes::people::customer_support_health),
            )
            .route(
                "/api/organizations",
                get(routes::people::list_organizations),
            )
            .route(
                "/api/organizations/:id",
                get(routes::people::get_organization),
            )
            .route(
                "/api/organizations/:id/timeline",
                get(routes::people::organization_timeline),
            )
            .route(
                "/api/organizations/:id/support-health",
                get(routes::people::organization_support_health),
            )
            .route(
                "/api/timeline/rebuild",
                post(routes::people::timeline_rebuild),
            )
            // Search
            .route("/api/search", post(routes::search::search))
            // Inbox views
            .route("/api/inbox-views", get(routes::views::list_views))
            .route("/api/inbox-views/:id", get(routes::views::get_view))
            .route("/api/inbox-views", post(routes::views::create_view))
            .route(
                "/api/inbox-views/:id",
                axum::routing::patch(routes::views::update_view),
            )
            .route(
                "/api/inbox-views/:id",
                axum::routing::delete(routes::views::delete_view),
            )
            .route(
                "/api/inbox-views/preview",
                post(routes::views::preview_view),
            )
            // Operations
            .route("/api/operations/center", get(routes::operations::center))
            .route(
                "/api/operations/workload",
                get(routes::operations::workload),
            )
            .route(
                "/api/operations/capacity",
                axum::routing::put(routes::operations::set_capacity),
            )
            .route(
                "/api/operations/waiting-threshold",
                axum::routing::put(routes::operations::set_waiting_threshold),
            )
            .route(
                "/api/operations/suggested-assignees",
                get(routes::operations::suggested_assignees),
            )
            // Notifications
            .route("/api/notifications", get(routes::notifications::list))
            .route(
                "/api/notifications/unread-count",
                get(routes::notifications::unread_count),
            )
            .route(
                "/api/notifications/:id/read",
                post(routes::notifications::mark_read),
            )
            .route(
                "/api/notifications/read-all",
                post(routes::notifications::mark_all_read),
            )
            .route(
                "/api/notifications/prefs",
                get(routes::notifications::list_prefs),
            )
            .route(
                "/api/notifications/prefs/:type",
                axum::routing::put(routes::notifications::set_pref),
            )
            .route(
                "/api/notifications/mentions",
                get(routes::notifications::mentions),
            )
            .route(
                "/api/notifications/sweep",
                post(routes::notifications::sweep),
            )
            // Settings
            .route("/api/settings", get(routes::settings::get_settings))
            .route(
                "/api/settings",
                axum::routing::patch(routes::settings::update_settings),
            )
            .route(
                "/api/settings/lmstudio",
                get(routes::settings::get_lmstudio),
            )
            .route(
                "/api/settings/lmstudio",
                axum::routing::patch(routes::settings::update_lmstudio),
            )
            .route(
                "/api/settings/lmstudio/test",
                post(routes::settings::test_lmstudio),
            )
            .route("/api/settings/qdrant", get(routes::settings::get_qdrant))
            .route(
                "/api/settings/qdrant",
                axum::routing::patch(routes::settings::update_qdrant),
            )
            .route(
                "/api/settings/qdrant/test",
                post(routes::settings::qdrant_test),
            )
            .route(
                "/api/settings/appearance",
                get(routes::settings::appearance),
            )
            .route(
                "/api/settings/business-hours",
                get(routes::settings::get_business_hours),
            )
            .route(
                "/api/settings/business-hours/:mailboxId",
                axum::routing::put(routes::settings::set_business_hours),
            )
            .route(
                "/api/settings/business-hours/:mailboxId",
                axum::routing::delete(routes::settings::delete_business_hours),
            )
            // Sync
            .route("/api/sync/status", get(routes::sync::status))
            .route("/api/sync/initial", post(routes::sync::initial))
            .route("/api/sync/incremental", post(routes::sync::incremental))
            .route("/api/sync/reconcile", post(routes::sync::reconcile))
            .route("/api/sync/cancel", post(routes::sync::cancel))
            .route("/api/sync/encrypted", get(routes::sync::encrypted_list))
            .route(
                "/api/sync/encrypted/export",
                post(routes::sync::encrypted_export),
            )
            .route(
                "/api/sync/encrypted/verify",
                post(routes::sync::encrypted_verify),
            )
            .route(
                "/api/sync/encrypted/import",
                post(routes::sync::encrypted_import),
            )
            .route(
                "/api/sync/encrypted/upload",
                post(routes::sync::encrypted_upload)
                    // Per-route override: bundles are whole encrypted
                    // databases and can be far larger than the JSON API
                    // limit (reference: 512 MB).
                    .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024)),
            )
            // OAuth flow (reference routes/sync.ts:269-385) — handlers live in
            // routes::oauth (audit-line port: provider-backed, me_remote_id
            // persistence, exact reference messages).
            // Audit log + application errors + backups
            .route("/api/audit", get(routes::system::audit_log))
            .route("/api/errors", get(routes::system::errors))
            .route("/api/backups", get(routes::system::backups_list))
            .route("/api/backups/create", post(routes::system::backups_create))
            .route(
                "/api/backups/export-json",
                post(routes::system::backups_export_json),
            )
            .route(
                "/api/backups/export-csv",
                post(routes::system::backups_export_csv),
            )
            .route(
                "/api/sync/rebuild-search-index",
                post(routes::sync::rebuild_search_index),
            )
            .route(
                "/api/sync/rebuild-embeddings",
                post(routes::sync::rebuild_embeddings),
            )
            .route(
                "/api/webhooks/register",
                post(routes::sync::register_webhook),
            )
            .route(
                "/api/webhooks/:remoteId",
                axum::routing::delete(routes::sync::unregister_webhook),
            )
            .route("/api/queue", get(routes::sync::queue))
            .route("/api/queue/:id/retry", post(routes::sync::retry_job))
            .route("/api/queue/:id/cancel", post(routes::sync::cancel_job))
            .route(
                "/api/queue/clear-completed",
                post(routes::sync::clear_completed),
            )
            // OAuth
            .route(
                "/api/oauth/authorize-url",
                get(routes::oauth::authorize_url),
            )
            .route(
                "/api/oauth/client-credentials",
                post(routes::oauth::client_credentials),
            )
            .route("/api/oauth/status", get(routes::oauth::status))
            .route("/api/oauth/disconnect", post(routes::oauth::disconnect))
            .route("/oauth/callback", get(routes::oauth::callback))
            // Webhook receiver
            .route("/api/webhooks/helpscout", post(routes::webhook::handle))
            // Analytics
            .route(
                "/api/analytics/dashboard",
                get(routes::analytics::dashboard),
            )
            .route("/api/analytics/ai", get(routes::analytics::ai_analytics))
            .route("/api/reports/sla", get(routes::analytics::sla_report))
            .route(
                "/api/reports/why-contacting",
                get(routes::analytics::why_contacting),
            )
            .route(
                "/api/reports/top-questions",
                get(routes::analytics::top_questions),
            )
            .route("/api/reports/doc-gaps", get(routes::analytics::doc_gaps))
            .route(
                "/api/reports/answer-reuse",
                get(routes::analytics::answer_reuse),
            )
            .route(
                "/api/reports/issue-radar",
                get(routes::analytics::issue_radar),
            )
            .route(
                "/api/reports/metric-definitions",
                get(routes::analytics::metric_definitions),
            )
            .route(
                "/api/reports/builder/run",
                post(routes::analytics::builder_run),
            )
            .route(
                "/api/reports/builder/saved",
                get(routes::analytics::builder_saved_list)
                    .post(routes::analytics::builder_saved_create),
            )
            .route(
                "/api/reports/builder/saved/:id",
                axum::routing::delete(routes::analytics::builder_saved_delete),
            )
            .route(
                "/api/reports/effectiveness",
                get(routes::analytics::effectiveness),
            )
            .route(
                "/api/reports/release-correlation",
                get(routes::analytics::release_correlation),
            )
            .route(
                "/api/reports/release-events",
                post(routes::analytics::release_events),
            )
            .route(
                "/api/reports/helpscout/:reportKey",
                get(routes::analytics::helpscout_report),
            )
            .route("/api/reports/narrative", post(routes::analytics::narrative))
            .route(
                "/api/reports/builder/catalog",
                get(routes::analytics::report_catalog),
            )
            // AI
            .route("/api/ai/status", get(routes::ai::status))
            .route("/api/ai/analyze/:conversationId", post(routes::ai::analyze))
            .route("/api/ai/draft/:conversationId", post(routes::ai::draft))
            .route("/api/ai/similar/:conversationId", get(routes::ai::similar))
            .route("/api/ai/memory/:customerId", get(routes::ai::get_memory))
            .route("/api/ai/memory/:customerId", post(routes::ai::set_memory))
            .route("/api/ai/jobs", get(routes::ai::jobs))
            .route("/api/ai/analytics", get(routes::ai::ai_analytics))
            .route("/api/ai/evaluation", get(routes::ai::evaluation))
            .route("/api/ai/cluster-issues", post(routes::ai::cluster_issues))
            .route("/api/ai/draft/:draftId/rewrite", post(routes::ai::rewrite))
            .route("/api/ai/draft/:draftId/verify", post(routes::ai::verify))
            .route(
                "/api/ai/draft/:draftId/feedback",
                post(routes::ai::feedback),
            )
            // Issues
            .route("/api/issues/clusters", get(routes::issues::list_clusters))
            .route("/api/issues/sla-alerts", get(routes::issues::sla_alerts))
            .route("/api/issues/known", get(routes::issues::list_known))
            .route("/api/issues/known", post(routes::issues::create_known))
            .route("/api/issues/known/:id", get(routes::issues::get_known))
            .route(
                "/api/issues/known/:id",
                axum::routing::patch(routes::issues::update_known),
            )
            .route(
                "/api/issues/known/:id",
                axum::routing::delete(routes::issues::delete_known),
            )
            .route(
                "/api/issues/known/:id/impact",
                get(routes::issues::known_impact),
            )
            .route("/api/issues/cases", get(routes::issues::list_cases))
            .route(
                "/api/issues/cases/from-conversation/:conversationId",
                post(routes::issues::case_from_conversation),
            )
            .route("/api/issues/clusters/:id", get(routes::issues::get_cluster))
            .route(
                "/api/issues/clusters/:id",
                axum::routing::delete(routes::issues::delete_cluster),
            )
            .route("/api/issues/known/:id/refs", post(routes::issues::add_ref))
            .route(
                "/api/issues/known/:id/link/:conversationId",
                post(routes::issues::link_known),
            )
            .route(
                "/api/issues/known/:id/link/:conversationId",
                axum::routing::delete(routes::issues::unlink_known),
            )
            // Automation
            .route("/api/automation/rules", get(routes::automation::list_rules))
            .route(
                "/api/automation/rules",
                post(routes::automation::create_rule),
            )
            .route(
                "/api/automation/rules/:id",
                axum::routing::patch(routes::automation::update_rule),
            )
            .route(
                "/api/automation/rules/:id",
                axum::routing::delete(routes::automation::delete_rule),
            )
            .route(
                "/api/automation/rules/:id/trigger/:conversationId",
                post(routes::automation::trigger),
            )
            // Collaboration (side threads)
            .route(
                "/api/conversations/:id/side-threads",
                get(routes::collaboration::list_side_threads),
            )
            .route(
                "/api/conversations/:id/side-threads",
                post(routes::collaboration::create_side_thread),
            )
            .route(
                "/api/side-threads/:id",
                get(routes::collaboration::get_side_thread),
            )
            .route(
                "/api/side-threads/:id/messages",
                post(routes::collaboration::add_message),
            )
            .route(
                "/api/side-threads/:id/resolve",
                post(routes::collaboration::resolve),
            )
            .route(
                "/api/side-threads/:id/reopen",
                post(routes::collaboration::reopen),
            )
            .route(
                "/api/side-threads/:id/participants",
                post(routes::collaboration::add_participants),
            )
            .route(
                "/api/mention-directory",
                get(routes::collaboration::mention_directory),
            )
            // Copilot
            .route("/api/copilot/sessions", get(routes::copilot::list_sessions))
            .route("/api/copilot/chat", post(routes::copilot::chat))
            .route(
                "/api/copilot/sessions/:id",
                get(routes::copilot::get_session),
            )
            .route(
                "/api/copilot/sessions/:id",
                axum::routing::delete(routes::copilot::delete_session),
            )
            .route("/api/copilot/tools", get(routes::copilot::tools))
            .route(
                "/api/copilot/starter-questions/:conversationId",
                get(routes::copilot::starter_questions),
            )
            // Knowledge
            .route(
                "/api/knowledge/sources",
                get(routes::knowledge::list_sources),
            )
            .route(
                "/api/knowledge/documents",
                get(routes::knowledge::list_documents),
            )
            .route(
                "/api/knowledge/documents/:id",
                get(routes::knowledge::get_document),
            )
            .route("/api/knowledge/search", get(routes::knowledge::search))
            .route(
                "/api/knowledge/freshness",
                get(routes::knowledge::freshness),
            )
            .route("/api/knowledge/reindex", post(routes::knowledge::reindex))
            .route("/api/knowledge/import", post(routes::knowledge::import))
            .route(
                "/api/knowledge/importable",
                get(routes::knowledge::importable),
            )
            .route(
                "/api/knowledge/import-file",
                post(routes::knowledge::import_file),
            )
            .route(
                "/api/knowledge/documents/:id",
                axum::routing::delete(routes::knowledge::delete_document),
            )
            .route(
                "/api/knowledge/documents/:id/review",
                post(routes::knowledge::review_document),
            )
            .route(
                "/api/knowledge/documents/:id/verify",
                post(routes::knowledge::verify_document),
            )
            // Outreach
            .route("/api/outreach/meta", get(routes::outreach::meta))
            .route(
                "/api/outreach/segments",
                get(routes::outreach::list_segments),
            )
            .route(
                "/api/outreach/segments",
                post(routes::outreach::create_segment),
            )
            .route(
                "/api/outreach/segments/:id",
                axum::routing::delete(routes::outreach::delete_segment),
            )
            .route(
                "/api/outreach/segments/preview",
                post(routes::outreach::preview_segment),
            )
            .route(
                "/api/outreach/segments/estimate",
                post(routes::outreach::estimate_segment),
            )
            .route(
                "/api/outreach/segments/suggest",
                post(routes::outreach::suggest_segment),
            )
            .route(
                "/api/outreach/campaigns",
                get(routes::outreach::list_campaigns),
            )
            .route(
                "/api/outreach/campaigns",
                post(routes::outreach::create_campaign),
            )
            .route(
                "/api/outreach/campaigns/:id",
                get(routes::outreach::get_campaign),
            )
            // Campaign lifecycle (reference outreach.ts:289-375)
            .route(
                "/api/outreach/campaigns/:id/validate",
                get(routes::outreach::campaign_validate_route),
            )
            .route("/api/outreach/render", post(routes::outreach::render_route))
            .route(
                "/api/outreach/campaigns/:id/preview",
                post(routes::outreach::campaign_preview_route),
            )
            .route(
                "/api/outreach/campaigns/:id/queue",
                post(routes::outreach::campaign_queue_route),
            )
            .route(
                "/api/outreach/campaigns/:id/pause",
                post(routes::outreach::campaign_pause_route),
            )
            .route(
                "/api/outreach/campaigns/:id/resume",
                post(routes::outreach::campaign_resume_route),
            )
            .route(
                "/api/outreach/campaigns/:id/cancel",
                post(routes::outreach::campaign_cancel_route),
            )
            .route(
                "/api/outreach/campaigns/:id/retry",
                post(routes::outreach::campaign_retry_route),
            )
            .route(
                "/api/outreach/campaigns/:id/reconcile",
                post(routes::outreach::campaign_reconcile_route),
            )
            .route(
                "/api/outreach/campaigns/:id/report",
                get(routes::outreach::campaign_report_route),
            )
            .route(
                "/api/outreach/campaigns/:id",
                axum::routing::delete(routes::outreach::campaign_delete_route),
            )
            // Do-Not-Contact (reference outreach.ts:377-405)
            .route("/api/outreach/dnc", get(routes::outreach::list_dnc_route))
            .route("/api/outreach/dnc", post(routes::outreach::add_dnc_route))
            .route(
                "/api/outreach/dnc/:customerLocalId",
                axum::routing::delete(routes::outreach::remove_dnc_route),
            )
            // Custom Objects
            .route(
                "/api/custom-objects/types",
                get(routes::custom_objects::list_types),
            )
            .route(
                "/api/custom-objects/types",
                post(routes::custom_objects::create_type),
            )
            .route(
                "/api/custom-objects/types/:id",
                get(routes::custom_objects::get_type),
            )
            .route(
                "/api/custom-objects/types/:id",
                axum::routing::patch(routes::custom_objects::update_type),
            )
            .route(
                "/api/custom-objects/types/:id",
                axum::routing::delete(routes::custom_objects::delete_type),
            )
            .route(
                "/api/custom-objects",
                get(routes::custom_objects::list_objects),
            )
            .route(
                "/api/custom-objects",
                post(routes::custom_objects::create_object),
            )
            .route(
                "/api/custom-objects/:id",
                get(routes::custom_objects::get_object),
            )
            .route(
                "/api/custom-objects/:id",
                axum::routing::patch(routes::custom_objects::update_object),
            )
            .route(
                "/api/custom-objects/:id",
                axum::routing::delete(routes::custom_objects::delete_object),
            )
            .route(
                "/api/custom-objects/report",
                get(routes::custom_objects::report),
            )
            .route(
                "/api/custom-objects/for/:targetKind/:targetId",
                get(routes::custom_objects::list_for_target),
            )
            .route(
                "/api/custom-objects/:id/links",
                post(routes::custom_objects::create_link),
            )
            .route(
                "/api/custom-objects/:id/links/:targetKind/:targetLocalId",
                axum::routing::delete(routes::custom_objects::delete_link),
            )
            // Connectors
            .route("/api/connectors", get(routes::connectors::list))
            .route("/api/connectors", post(routes::connectors::create))
            .route("/api/connectors/:id", get(routes::connectors::get))
            .route(
                "/api/connectors/:id",
                axum::routing::patch(routes::connectors::update),
            )
            .route(
                "/api/connectors/:id",
                axum::routing::delete(routes::connectors::delete),
            )
            .route(
                "/api/connectors/:id/refresh",
                post(routes::connectors::refresh),
            )
            .route("/api/connectors/:id/rows", get(routes::connectors::rows))
            .route("/api/connectors/:id/test", post(routes::connectors::test))
            // Incidents
            .route("/api/incidents", get(routes::incidents::list))
            .route("/api/incidents", post(routes::incidents::create))
            .route("/api/incidents/:id", get(routes::incidents::get))
            .route(
                "/api/incidents/:id",
                axum::routing::patch(routes::incidents::update),
            )
            .route(
                "/api/incidents/:id",
                axum::routing::delete(routes::incidents::delete),
            )
            .route("/api/incidents/:id/impact", get(routes::incidents::impact))
            .route(
                "/api/incidents/:id/notes",
                post(routes::incidents::add_note),
            )
            .route("/api/incidents/:id/refs", post(routes::incidents::add_ref))
            .route(
                "/api/incidents/:id/related",
                post(routes::incidents::add_related),
            )
            .route(
                "/api/incidents/:id/related/:targetKind/:targetLocalId",
                axum::routing::delete(routes::incidents::delete_related),
            )
            .route(
                "/api/incidents/:id/releases",
                post(routes::incidents::add_release),
            )
            .route(
                "/api/incidents/:id/releases/:releaseId",
                axum::routing::delete(routes::incidents::delete_release),
            )
            .route(
                "/api/incidents/:id/conversations/:conversationId",
                post(routes::incidents::link_conversation),
            )
            .route(
                "/api/incidents/:id/conversations/:conversationId",
                axum::routing::delete(routes::incidents::unlink_conversation),
            )
            .route(
                "/api/incidents/:id/refs/:refId",
                axum::routing::delete(routes::incidents::delete_ref),
            )
            .route(
                "/api/incidents/from-cluster/:clusterId",
                post(routes::incidents::from_cluster),
            )
            .route(
                "/api/incidents/from-known-issue/:knownIssueId",
                post(routes::incidents::from_known_issue),
            )
            // Graph
            .route("/api/graph/stats", get(routes::graph::stats))
            .route("/api/graph/meta", get(routes::graph::meta))
            .route("/api/graph/search", get(routes::graph::search))
            .route("/api/graph/node/:kind/:id", get(routes::graph::node))
            .route(
                "/api/graph/neighbors/:kind/:id",
                get(routes::graph::neighbors),
            )
            .route(
                "/api/graph/subgraph/:kind/:id",
                get(routes::graph::subgraph),
            )
            .route("/api/graph/edges", get(routes::graph::list_edges))
            .route("/api/graph/edges", post(routes::graph::create_edge))
            .route(
                "/api/graph/edges/:id",
                axum::routing::delete(routes::graph::delete_edge),
            )
            // Attributes
            .route("/api/attributes/catalog", get(routes::attributes::catalog))
            .route(
                "/api/attributes/conversation/:id",
                get(routes::attributes::conversation_attributes),
            )
            .route(
                "/api/attributes/conversation/:id/history/:attribute",
                get(routes::attributes::conversation_attribute_history),
            )
            .route(
                "/api/attributes/conversation/:id/recompute",
                post(routes::attributes::recompute),
            )
            .route(
                "/api/attributes/conversations",
                get(routes::attributes::conversations),
            )
            .route("/api/attributes/report", get(routes::attributes::report))
            .route(
                "/api/attributes/values/:attribute",
                get(routes::attributes::values),
            )
            // Coaching
            .route("/api/coaching/meta", get(routes::coaching::meta))
            .route(
                "/api/coaching/:conversationId",
                get(routes::coaching::get_coaching),
            )
            .route(
                "/api/coaching/:conversationId/review",
                post(routes::coaching::review),
            )
            // Translation
            .route("/api/translation/meta", get(routes::translation::meta))
            .route("/api/translation/detect", post(routes::translation::detect))
            .route(
                "/api/translation/conversation/:id",
                get(routes::translation::conversation),
            )
            .route(
                "/api/translation/translate",
                post(routes::translation::translate),
            )
            // Memory
            .route("/api/memory/meta", get(routes::memory::meta))
            .route("/api/memory/:customerId", get(routes::memory::get))
            .route(
                "/api/memory/:customerId/entries",
                post(routes::memory::add_entry),
            )
            .route(
                "/api/memory/:customerId/entries/:entryId",
                axum::routing::delete(routes::memory::delete_entry),
            )
            // Interactions
            .route(
                "/api/interaction/:conversationId",
                get(routes::interactions::get),
            )
            .route(
                "/api/interaction/:conversationId/refresh",
                post(routes::interactions::refresh),
            )
            .route(
                "/api/interaction/:conversationId/evidence",
                get(routes::interactions::evidence),
            )
            .route(
                "/api/interaction/profile/:customerId",
                get(routes::interactions::profile),
            )
            .route(
                "/api/interaction/profile/:customerId/override",
                post(routes::interactions::set_override),
            )
            .route(
                "/api/interaction/profile/:customerId/override/:field",
                axum::routing::delete(routes::interactions::clear_override),
            )
            // Quality (QA, friction, gaps)
            .route("/api/knowledge/gaps", get(routes::quality::list_gaps))
            .route(
                "/api/knowledge/gaps/rebuild",
                post(routes::quality::rebuild_gaps),
            )
            .route(
                "/api/knowledge/gaps/candidates/:id/decide",
                post(routes::quality::decide_candidate),
            )
            .route(
                "/api/knowledge/gaps/candidates/:id/draft",
                get(routes::quality::draft_candidate),
            )
            .route("/api/qa/overview", get(routes::quality::qa_overview))
            .route("/api/qa/rebuild", post(routes::quality::qa_rebuild))
            .route(
                "/api/qa/:conversationId",
                get(routes::quality::qa_conversation),
            )
            .route(
                "/api/qa/:conversationId/analyze",
                post(routes::quality::qa_analyze),
            )
            .route(
                "/api/friction/overview",
                get(routes::quality::friction_overview),
            )
            .route(
                "/api/friction/rebuild",
                post(routes::quality::friction_rebuild),
            )
            .route(
                "/api/friction/:conversationId",
                get(routes::quality::friction_conversation),
            )
            // Docs
            .route("/api/docs/collections", get(routes::docs::collections))
            .route("/api/docs/stats", get(routes::docs::stats))
            .route("/api/docs/search", get(routes::docs::search))
            .route("/api/docs/articles", get(routes::docs::articles))
            .route("/api/docs/articles/:id", get(routes::docs::article))
            // 404 fallback for unknown /api/* routes
            .fallback(any(routes::not_found))
            .with_state(state)
            // Fastify parity: a request whose PATH matches a registered
            // route but whose METHOD does not gets the 404 "Unknown API
            // endpoint." body in the reference (Fastify's router has no
            // 405 path); axum answers 405 by default. Rewrite 405 -> 404
            // with the reference body so wrong-method probes observe the
            // same status + JSON.
            .layer(axum::middleware::from_fn(rewrite_405_to_404))
            // Request body limit: 20 MB — the reference's Fastify
            // `bodyLimit: 20 * 1024 * 1024` (attachment uploads are
            // base64-inflated). The .sosync octet-stream upload route
            // overrides this per-route with 512 MB.
            .layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024))
            .layer(cors)
            .layer(TraceLayer::new_for_http())
            .layer(rate_limit)
            .layer(dns_guard)
    }

    /// Run the server forever.
    pub async fn serve(mut self) -> std::io::Result<()> {
        // 9. background workers (spec #99: start them before the listener).
        //    Jobs survive restarts; timers never block request handling.
        let provider: Arc<dyn crate::helpscout::HelpScoutProvider> = match self.state.provider_kind.as_str() {
            "real" => self.state.real.clone().map(|r| r as Arc<dyn crate::helpscout::HelpScoutProvider>).unwrap_or_else(|| Arc::new(crate::helpscout::FakeHelpScoutProvider::new_demo())),
            _ => Arc::new(crate::helpscout::FakeHelpScoutProvider::new_demo()),
        };
        let manager = crate::workers::start_workers(
            self.state.conn.clone(),
            self.state.sync.clone(),
            provider,
            self.state.bus.clone(),
            self.state.data_dir.clone(),
        );
        // Demo mode with an empty database: run the initial demo sync
        // automatically (reference index.ts) — the seed pass lands with the
        // demo module; the engine's mirror pass is enough for the timer
        // machinery to be exercised end-to-end.
        if self.state.provider_kind == "fake" {
            let conversation_count: i64 = {
                let conn = self.state.conn_lock();
                conn.query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
                    .unwrap_or(0)
            };
            if conversation_count == 0 {
                if let Some(engine) = self.state.sync.clone() {
                    let conn = self.state.conn.clone();
                    tokio::spawn(async move {
                        if let Err(e) = engine.initial_sync().await {
                            tracing::warn!(error = %e, "Demo initial sync failed");
                            return;
                        }
                        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                        let _ = crate::jobs::enqueue_on(&c, "embeddings", "embed_knowledge_chunks", "{}", 4);
                        let seeded = crate::demo::seed_demo_data(&c);
                        if seeded {
                            let _ = crate::settings::set_string(&c, "demo_data_loaded", "true");
                        }
                    });
                }
            }
        }
        let workers_handle = manager.clone();
        self.state.workers = Some(manager);
        // into_make_service_with_connect_info lets the rate-limit layer
        // read the client's socket address via ConnectInfo<SocketAddr>.
        // (Mirrors the reference's `request.socket.remoteAddress`.)
        let app = self
            .build_router()
            .into_make_service_with_connect_info::<std::net::SocketAddr>();
        let listener = TcpListener::bind(self.addr).await?;
        tracing::info!(addr = %self.addr, "HTTP API server bound");
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = tokio::signal::ctrl_c().await;
                // v2.2.1 audit fix: stop the workers so in-flight ticks end
                // on the process's own terms.
                workers_handle.stop();
            })
            .await?;
        Ok(())
    }
}

/// Fastify parity middleware: rewrite axum's 405 (method not allowed) to
/// the reference's 404 "Unknown API endpoint." response — Fastify's router
/// has no 405 path, so wrong-method probes must observe 404.
async fn rewrite_405_to_404(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let resp = next.run(req).await;
    if resp.status() == axum::http::StatusCode::METHOD_NOT_ALLOWED {
        return (
            axum::http::StatusCode::NOT_FOUND,
            [("Content-Type", "application/json")],
            r#"{"statusCode":404,"error":"NotFound","message":"Unknown API endpoint."}"#,
        )
            .into_response();
    }
    resp
}

/// Check if a Host header is a loopback address.
/// Matches the reference's isLoopbackHostHeader function.
fn is_loopback_host(host: &str) -> bool {
    if host.is_empty() {
        return true; // HTTP/1.0-style requests carry no Host
    }
    let mut h = host.to_lowercase();
    // Strip optional :port without breaking IPv6 literals ([::1]:3000).
    if !h.ends_with(']') {
        if let Some(idx) = h.rfind(':') {
            if idx > 0 && h[idx + 1..].chars().all(|c| c.is_ascii_digit()) {
                h.truncate(idx);
            }
        }
    }
    h = h.trim_start_matches('[').trim_end_matches(']').to_string();
    h == "localhost" || h == "127.0.0.1" || h == "::1" || h == "::ffff:127.0.0.1"
}
