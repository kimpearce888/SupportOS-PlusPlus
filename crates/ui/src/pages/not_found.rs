//! Not-found page — the fallback route.
//!
//! Per spec KNOWN PITFALLS + A12: every route renders a state. Unknown routes
//! get a clear not-found state with the bad path echoed.

use leptos::*;

/// The not-found page. `path` is a getter closure so the router can pass
/// reactive params; it returns the unmatched path string.
#[component]
pub fn NotFoundPage<F>(path: F) -> impl IntoView
where
    F: Fn() -> String + 'static,
{
    view! {
        <div class="spp-not-found">
            <p class="spp-not-found__code">"404"</p>
            <h2 class="spp-state__title">"Page not found"</h2>
            <p class="spp-state__body">
                "The route " <code>{path}</code> " does not exist."
            </p>
        </div>
    }
}
