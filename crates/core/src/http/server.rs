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

use super::routes;

/// Shared application state for all HTTP handlers.
/// This wraps the SQLite connection + config, similar to the reference's AppContext.
#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<std::sync::Mutex<rusqlite::Connection>>,
    pub data_dir: std::path::PathBuf,
    pub port: u16,
    pub host: String,
    pub demo_mode: bool,
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

        // CORS: localhost-only (matching the reference's allowedOrigins).
        let cors = CorsLayer::new()
            .allow_origin([
                format!("http://localhost:{}", self.state.port)
                    .parse()
                    .unwrap(),
                format!("http://127.0.0.1:{}", self.state.port)
                    .parse()
                    .unwrap(),
                "http://localhost:5173".parse().unwrap(),
                "http://localhost:5174".parse().unwrap(),
                "http://localhost:5175".parse().unwrap(),
                "http://127.0.0.1:5173".parse().unwrap(),
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
                get(routes::conversations::list_ticket_states),
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
                "/api/reports/effectiveness",
                get(routes::analytics::effectiveness),
            )
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
            .route("/api/issues/cases", get(routes::issues::list_cases))
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
            .route("/api/attributes/report", get(routes::attributes::report))
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
            // Quality (QA, friction, gaps)
            .route("/api/knowledge/gaps", get(routes::quality::list_gaps))
            .route(
                "/api/knowledge/gaps/rebuild",
                post(routes::quality::rebuild_gaps),
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
            // Docs
            .route("/api/docs/collections", get(routes::docs::collections))
            .route("/api/docs/stats", get(routes::docs::stats))
            .route("/api/docs/search", get(routes::docs::search))
            .route("/api/docs/articles", get(routes::docs::articles))
            .route("/api/docs/articles/:id", get(routes::docs::article))
            // 404 fallback for unknown /api/* routes
            .fallback(any(routes::not_found))
            .with_state(state)
            .layer(cors)
            .layer(TraceLayer::new_for_http())
    }

    /// Run the server forever.
    pub async fn serve(self) -> std::io::Result<()> {
        let app = self.build_router();
        let listener = TcpListener::bind(self.addr).await?;
        tracing::info!(addr = %self.addr, "HTTP API server bound");
        axum::serve(listener, app).await?;
        Ok(())
    }
}
