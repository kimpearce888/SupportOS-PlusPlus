//! Layout shell — Topbar + Main + Footer.
//!
//! Per A11 (visual reference): the reference repo uses a top bar with the
//! product name + a main content area + a footer with the legal notice.
//! We match the structure, not the CSS.

use leptos::*;
use leptos_router::*;

/// The layout shell. Renders the topbar (with nav), the `<main>` slot for
/// the routed page, and the footer.
#[component]
pub fn LayoutShell() -> impl IntoView {
    view! {
        <div class="spp-shell">
            <header class="spp-topbar">
                <h1 class="spp-topbar__title">"SupportOS++"</h1>
                <span class="spp-topbar__version">"v0.1.0 — M2"</span>
                <nav class="spp-nav">
                    <A href="/" class="spp-nav__link" active_class="spp-nav__link--active">
                        "Dashboard"
                    </A>
                    <A href="/sync-health" class="spp-nav__link" active_class="spp-nav__link--active">
                        "Sync Health"
                    </A>
                    <A href="/settings" class="spp-nav__link" active_class="spp-nav__link--active">
                        "Settings"
                    </A>
                </nav>
            </header>
            <main class="spp-main">
                <Outlet />
            </main>
            <footer class="spp-footer">
                <p>
                    "Help Scout is a trademark of Help Scout, Inc. "
                    "SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout."
                </p>
            </footer>
        </div>
    }
}
