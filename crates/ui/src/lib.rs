//! SupportOS++ UI — Leptos/WASM frontend.
//!
//! CSR-only build. Bundled by `trunk` into `dist/` and served by Tauri.
//! No hand-written JS/TS anywhere (spec hard rule).
//!
//! # Routes
//!
//! - `/` — Dashboard page (placeholder for M1; real KPI tiles land in M3).
//! - `/settings` — Settings page (placeholder; real settings land in M2+).
//! - `*` — Not-found page (per KNOWN PITFALLS: every route renders a state).

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use leptos::*;
use leptos_router::*;

pub mod components;
pub mod layout;
pub mod pages;

// Re-export the catalog so the UI has type-safe access to closed
// vocabularies (one source of truth per spec A12). The catalog crate is
// WASM-safe (no I/O deps).
pub use spp_catalog as catalog;

/// Mount the app at the `<div id="app">` element. Called by `index.html`.
pub fn mount() {
    console_error_panic_hook::set_once();
    mount_to_body(app_view);
}

fn app_view() -> impl IntoView {
    // The first-run onboarding overlay. The actual `first_run_done` flag is
    // read from the DB via the Tauri IPC command `first_run_state`; for M1
    // we wire it as a signal that starts as `false` (the DB default) and is
    // set to `true` when the user picks either action. M2 will replace the
    // initial value with a real Tauri IPC call.
    let first_run_done = create_rw_signal(false);

    view! {
        <Router>
            <Routes>
                <Route path="/" view=layout::LayoutShell>
                    <Route path="/" view=pages::DashboardPage />
                    <Route path="/settings" view=pages::SettingsPage />
                    <Route path="/*any" view=not_found_from_params />
                </Route>
            </Routes>
        </Router>
        <Show when=move || !first_run_done.get() fallback=|| ()>
            <components::OnboardingOverlay
                props=components::OnboardingProps::new(
                    false,
                    move || first_run_done.set(true),
                    move || first_run_done.set(true),
                )
            />
        </Show>
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
}
