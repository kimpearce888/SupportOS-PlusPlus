//! Dashboard page — the `/` route.
//!
//! M1: shows a placeholder dashboard that demonstrates the loading/empty/error
//! state pattern (KNOWN PITFALL: "every view has loading, empty, and error
//! states"). Real KPI tiles arrive in M3.

use leptos::*;

use crate::components::button::ButtonStyle;
use crate::components::{Button, StateView, ViewState};

/// The dashboard page.
#[component]
pub fn DashboardPage() -> impl IntoView {
    // Cycle through Loading → Empty → Loaded → Error → Loading to demonstrate
    // the state pattern. M3 replaces this with a real data fetch.
    let state = create_rw_signal(ViewState::loading());

    let cycle = move |_| {
        state.update(|s| {
            *s = match s {
                ViewState::Loading => ViewState::empty("No conversations synced yet."),
                ViewState::Empty { .. } => ViewState::Loaded,
                ViewState::Loaded => ViewState::error("Simulated fetch failure."),
                ViewState::Error { .. } => ViewState::loading(),
            };
        });
    };

    view! {
        <div class="spp-page spp-page--dashboard">
            <h2 class="spp-page__title">"Dashboard"</h2>
            <Button on_click=move || cycle(()) style=ButtonStyle::Ghost>
                "Cycle state"
            </Button>
            <StateView state=state>
                <div class="spp-page__body">
                    <p>"Loaded — KPI tiles will appear here in M3 (Activity engine & inbox)."</p>
                </div>
            </StateView>
        </div>
    }
}
