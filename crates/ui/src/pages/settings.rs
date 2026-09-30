//! Settings page — the `/settings` route.
//!
//! M1: shows the placeholder. Real settings UI arrives in M2 (Help Scout
//! credentials) and later milestones.

use leptos::*;

use crate::components::state_view::{EmptyState, ViewState};

/// The settings page.
#[component]
pub fn SettingsPage() -> impl IntoView {
    // For M1, settings is always "empty" — real settings UI lands in M2+.
    view! {
        <div class="spp-page spp-page--settings">
            <h2 class="spp-page__title">"Settings"</h2>
            <EmptyState message="Settings UI arrives in M2 (Help Scout credentials) and later milestones." />
        </div>
    }
}

// Suppress unused-import warning: ViewState is the canonical type for state
// handling, but SettingsPage renders an EmptyState directly for M1.
#[allow(dead_code)]
fn _view_state_for_documentation() -> ViewState {
    ViewState::empty("documentation placeholder")
}
