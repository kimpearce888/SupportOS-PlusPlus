//! SupportOS++ UI — Leptos/WASM frontend.
//!
//! CSR-only build. Bundled by `trunk` into `dist/` and served by Tauri.
//! No hand-written JS/TS anywhere (spec hard rule).

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use leptos::*;

/// Mount the app at the given DOM element id (default `#app`).
pub fn mount() {
    console_error_panic_hook::set_once();
    mount_to_body(view_fn);
}

fn view_fn() -> impl IntoView {
    let count = create_rw_signal(0i32);
    view! {
        <div class="spp-shell">
            <header class="spp-topbar">
                <h1 class="spp-title">"SupportOS++"</h1>
                <span class="spp-version">"v0.1.0 — M1 foundation"</span>
            </header>
            <main class="spp-main">
                <section class="spp-empty">
                    <p>"M1 foundation scaffold. The Help Scout mirror lands in M2."</p>
                    <button
                        class="spp-button"
                        on:click=move |_| count.update(|c| *c += 1)
                    >
                        "Counter: " {count}
                    </button>
                </section>
            </main>
            <footer class="spp-footer">
                <p>"Help Scout is a trademark of Help Scout, Inc. "
                    "SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout."
                </p>
            </footer>
        </div>
    }
}

// Re-export core types so the UI never depends on the Tauri shell crate directly.
pub use spp_core::catalog;

/// Re-export the core crate's public API under the `spp_core` alias.
extern crate spp_core as _core;
