//! SupportOS++ UI — Leptos/WASM frontend.
//!
//! CSR-only build. Bundled by `trunk` into `dist/` and served by Tauri.
//! No hand-written JS/TS anywhere (spec hard rule).
//!
//! # App shell (reference `App.tsx` parity)
//!
//! - Boot: `#[wasm_bindgen(start)]` mounts the app and applies the persisted
//!   theme before the first render (reference: `main.tsx` + uiStore read).
//! - Routes: the reference route table exactly — 26 routes including
//!   `/onboarding` (outside the shell) and the `/*any` 404.
//! - App-level effects (`AppEffects`, the reference's query/guard/bridge
//!   wiring): first-run onboarding guard, nav badge counts (30s poll +
//!   SSE-refreshed), one app-level SSE subscription, and the global
//!   keyboard shortcuts (Cmd/Ctrl+K palette, `/` quick search, g+d/g+i/g+s).
//! - Shell: sidebar (20 items, Directory/Intelligence/Operations sections,
//!   collapse toggle, Command + theme footer) — see `layout.rs`.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use leptos::*;
use leptos_router::*;

pub mod api;
pub mod components;
pub mod layout;
pub mod pages;
pub mod shortcuts;
pub mod sse;
pub mod state;
pub mod toasts;

// Re-export the catalog so the UI has type-safe access to closed
// vocabularies (one source of truth per spec A12). The catalog crate is
// WASM-safe (no I/O deps).
pub use spp_catalog as catalog;

/// WASM entry point. Trunk's generated bootstrap only *instantiates* the
/// module (it calls wasm-bindgen's `init()`; it does not invoke any exported
/// function), so the app must self-start here — the Leptos counterpart of
/// the reference's `main.tsx` `createRoot().render(...)`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    mount();
}

/// Mount the app at the `<div id="app">` element.
pub fn mount() {
    console_error_panic_hook::set_once();
    // Apply the persisted theme BEFORE the first render so the right token
    // set is active when the app mounts (no theme flash).
    state::apply_theme_at_boot();
    mount_to_body(app_view);
}

fn app_view() -> impl IntoView {
    let ui = state::UiState::create();
    provide_context(ui);
    // The global toast stack (reference uiStore toasts) — created before
    // the shell mounts so any component can `toasts::push` from the start.
    toasts::init();

    view! {
        <Router>
            <AppEffects />
            <Routes>
                // The onboarding wizard renders OUTSIDE the shell (reference
                // v1.6.0 audit fix: nav items around the wizard were dead).
                <Route path="/onboarding" view=pages::OnboardingPage />
                <Route path="/" view=layout::LayoutShell>
                    <Route path="/" view=pages::DashboardPage />
                    <Route path="/inbox" view=InboxListRoute />
                    <Route path="/inbox/conversation/:id" view=InboxRoute />
                    <Route path="/search" view=pages::SearchPage />
                    <Route path="/customers" view=pages::CustomerSearchPage />
                    <Route path="/customers/:id" view=CustomerProfileRoute />
                    <Route path="/organizations" view=pages::OrganizationsPage />
                    <Route path="/organizations/:id" view=OrganizationDetailRoute />
                    <Route path="/ai" view=pages::AiCenterPage />
                    <Route path="/issues" view=pages::IssueRadarPage />
                    <Route path="/incidents" view=pages::IncidentsPage />
                    <Route path="/incidents/:id" view=pages::IncidentsPage />
                    <Route path="/custom-objects" view=pages::CustomObjectsPage />
                    <Route path="/connectors" view=pages::ConnectorsPage />
                    <Route path="/graph" view=pages::SupportGraphPage />
                    <Route path="/knowledge" view=pages::KnowledgePage />
                    <Route path="/docs" view=pages::DocsPage />
                    <Route path="/reports" view=pages::ReportsPage />
                    <Route path="/outreach" view=pages::OutreachPage />
                    <Route path="/operations" view=pages::OperationsPage />
                    <Route path="/notifications" view=pages::NotificationsPage />
                    <Route path="/automation" view=pages::AutomationPage />
                    <Route path="/sync-health" view=pages::SyncHealthPage />
                    <Route path="/settings" view=pages::SettingsPage />
                    <Route path="/*any" view=not_found_from_params />
                </Route>
            </Routes>
            // Reference App.tsx mount order: routes, then <Toasts />, then
            // the event bridge, then the command palette.
            <toasts::Toasts />
            <components::CommandPalette
                open=ui.palette_open
                on_close=move || ui.palette_open.set(false)
            />
            <div class="spp-shortcut-bar" aria-hidden="true">
                <span><span class="spp-kbd">"⌘K"</span> " search"</span>
                <span><span class="spp-kbd">"/"</span> " quick search"</span>
                <span><span class="spp-kbd">"g"</span><span class="spp-kbd">"i"</span> " inbox"</span>
                <span><span class="spp-kbd">"g"</span><span class="spp-kbd">"d"</span> " dashboard"</span>
            </div>
        </Router>
    }
}

/// App-level effects, mounted once inside the Router (it needs the location
/// and navigate hooks). The Leptos counterpart of the reference App's
/// onboarding-guard query, nav-count queries, and `ServerEventsBridge`.
#[component]
fn AppEffects() -> impl IntoView {
    let ui = use_context::<state::UiState>()
        .expect("AppEffects requires the UiState context (provided by app_view)");
    let location = use_location();
    let navigate = use_navigate();

    // ── First-run guard (spec #100) ─────────────────────────────────────
    // Fetch /api/onboarding once. While it says !completed, any navigation
    // outside /onboarding is bounced back with history replaced. On fetch
    // failure the status stays unknown: no redirect and the shell stays
    // visible, like the reference's `undefined` query data.
    {
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(v) = crate::api::get_json::<serde_json::Value>("/api/onboarding").await {
                let completed = v.get("completed").and_then(|c| c.as_bool());
                ui.onboarding_completed.set(completed);
            }
        });
    }
    {
        let pathname = location.pathname;
        let navigate = navigate.clone();
        create_effect(move |_| {
            if ui.onboarding_completed.get() == Some(false) && pathname.get() != "/onboarding" {
                navigate(
                    "/onboarding",
                    NavigateOptions {
                        replace: true,
                        ..Default::default()
                    },
                );
            }
        });
    }

    // ── Nav badge counts (reference: 30s refetchInterval) ───────────────
    ui.refresh_nav_counts();
    set_interval(
        move || ui.refresh_nav_counts(),
        std::time::Duration::from_secs(30),
    );

    // ── SSE bridge (reference ServerEventsBridge) ───────────────────────
    // One app-level subscription opens the shared EventSource for the whole
    // app (pages must not subscribe per-page). The browser reconnects the
    // stream automatically; live events that can move the badge counts
    // trigger an immediate refresh (the reference invalidates the
    // 'nav-counts'/'notification-unread' queries on the same events).
    {
        let _unused_unsubscribe = crate::sse::subscribe(Box::new(move |event| match event {
            crate::sse::LiveEvent::NotificationReceived { .. }
            | crate::sse::LiveEvent::ConversationUpdated { .. }
            | crate::sse::LiveEvent::SyncCompleted { .. } => ui.refresh_nav_counts(),
            _ => {}
        }));
    }

    // ── Global keyboard shortcuts (spec #94) ────────────────────────────
    // `pending_g` is the armed state of the g+d/g+i/g+s sequence.
    let pending_g = std::rc::Rc::new(std::cell::Cell::new(false));
    view! {
        // An always-mounted element so the window listener lives for the
        // app lifetime (Leptos ties on:window:* listeners to the element).
        <div
            class="spp-app-effects"
            on:window:keydown=move |e: leptos::ev::KeyboardEvent| {
                let outcome = shortcuts::resolve(
                    pending_g.get(),
                    shortcuts::is_typing(e.target().as_ref()),
                    (e.meta_key() || e.ctrl_key()) && e.key().eq_ignore_ascii_case("k"),
                    &e.key(),
                );
                pending_g.set(outcome.pending_g);
                match outcome.action {
                    shortcuts::ShortcutAction::None => {}
                    shortcuts::ShortcutAction::OpenPalette => {
                        e.prevent_default();
                        ui.palette_open.set(true);
                    }
                    shortcuts::ShortcutAction::QuickSearch => {
                        e.prevent_default();
                        navigate("/search", Default::default());
                    }
                    shortcuts::ShortcutAction::Navigate(to) => {
                        navigate(to, Default::default());
                    }
                }
            }
        ></div>
    }
}

/// `/inbox` — the inbox without a preselected conversation (reference:
/// `id` from useParams is null on the base route).
#[component]
fn InboxListRoute() -> impl IntoView {
    view! { <pages::InboxPage /> }
}

/// `/inbox/conversation/:id` — reads the id from the route params and remounts
/// the inbox with that conversation selected (reference: `useParams().id`
/// drives the selected conversation; both routes share the InboxPage).
#[component]
fn InboxRoute() -> impl IntoView {
    let params = use_params_map();
    view! {
        {move || {
            let raw = params.with(|p| p.get("id").cloned().unwrap_or_default());
            // The reference guards with Number.isFinite(Number(id)).
            match raw.parse::<i64>() {
                Ok(id) => {
                    view! { <pages::InboxPage conversation_id=id /> }
                }
                Err(_) => {
                    view! { <pages::InboxPage /> }
                }
            }
        }}
    }
}

/// `/customers/:id` — reads the id from the route params and remounts the
/// profile when it changes (e.g. graph links between customers).
#[component]
fn CustomerProfileRoute() -> impl IntoView {
    let params = use_params_map();
    view! {
        {move || {
            let raw = params.with(|p| p.get("id").cloned().unwrap_or_default());
            let id = raw.parse::<i64>().unwrap_or(0);
            view! { <pages::CustomerProfilePage customer_id=id /> }
        }}
    }
}

/// `/organizations/:id` — same param pattern as the customer profile.
#[component]
fn OrganizationDetailRoute() -> impl IntoView {
    let params = use_params_map();
    view! {
        {move || {
            let raw = params.with(|p| p.get("id").cloned().unwrap_or_default());
            let id = raw.parse::<i64>().unwrap_or(0);
            view! { <pages::OrganizationDetailPage organization_id=id /> }
        }}
    }
}

/// Renders the not-found page, extracting the unmatched path from the router
/// params. Per KNOWN PITFALLS: every route renders a state.
fn not_found_from_params() -> impl IntoView {
    let params = use_params_map();
    let path = move || {
        params.with(|p| {
            let any = p.get("any").cloned().unwrap_or_default();
            if any.is_empty() {
                "/".to_string()
            } else {
                format!("/{any}")
            }
        })
    };
    view! { <pages::NotFoundPage path=path /> }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_reachable_from_ui() {
        // Smoke check: the catalog enum is reachable from the UI crate.
        // This catches accidental `pub use` removals.
        use catalog::OperationsTileKey;
        assert_eq!(OperationsTileKey::ALL.len(), 16);
    }

    #[test]
    fn reference_route_table_is_fully_wired() {
        // Every <Route> in the reference App.tsx must exist in this file's
        // app_view route table. Pinned against the source text so an
        // accidental route removal fails the build (reviewers diff the list
        // against App.tsx's <Route> entries).
        let source = include_str!("lib.rs");
        let reference_routes = [
            "/", // DashboardPage
            "/inbox",
            "/inbox/conversation/:id",
            "/search",
            "/customers",
            "/customers/:id",
            "/organizations",
            "/organizations/:id",
            "/ai",
            "/issues",
            "/incidents",
            "/incidents/:id",
            "/custom-objects",
            "/connectors",
            "/graph",
            "/knowledge",
            "/docs",
            "/reports",
            "/outreach",
            "/operations",
            "/notifications",
            "/automation",
            "/sync-health",
            "/settings",
            "/onboarding",
            "/*any", // 404
        ];
        assert_eq!(reference_routes.len(), 26, "reference has 26 routes");
        for route in reference_routes {
            let pattern = format!("path=\"{route}\" ");
            assert!(
                source.contains(&pattern),
                "route {route} is missing from the app_view route table"
            );
        }
    }
}
