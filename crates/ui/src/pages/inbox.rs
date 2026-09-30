//! Inbox page — the 3-pane layout (M3-T08).
//!
//! Per spec M3: "inbox." Per A11: visual reference is the reference repo's
//! `inbox.png` screenshot — 3-pane layout (list + detail + context).
//! Per KNOWN PITFALLS: "every view has loading, empty and error states."

use leptos::*;

use crate::components::state_view::EmptyState;

/// The inbox page — 3-pane layout:
/// 1. List pane: conversation list (filtered by saved view).
/// 2. Detail pane: conversation threads.
/// 3. Context pane: customer info + AI attributes.
#[component]
pub fn InboxPage() -> impl IntoView {
    let selected_conversation = create_rw_signal(None::<i64>);

    view! {
        <div class="spp-inbox">
            // ── List pane (left) ──
            <aside class="spp-inbox__list">
                <div class="spp-inbox__list-header">
                    <h2 class="spp-inbox__title">"Inbox"</h2>
                    <span class="spp-inbox__count">"0 conversations"</span>
                </div>
                <div class="spp-inbox__list-body">
                    <EmptyState message="No conversations synced yet. Connect Help Scout or try demo mode to see conversations here." />
                </div>
            </aside>

            // ── Detail pane (center) ──
            <main class="spp-inbox__detail">
                <Show
                    when=move || selected_conversation.get().is_some()
                    fallback=|| {
                        view! {
                            <EmptyState message="Select a conversation from the list to view its details." />
                        }
                    }
                >
                    <div class="spp-inbox__detail-content">
                        <p>"Conversation detail will appear here."</p>
                    </div>
                </Show>
            </main>

            // ── Context pane (right) ──
            <aside class="spp-inbox__context">
                <Show
                    when=move || selected_conversation.get().is_some()
                    fallback=|| {
                        view! {
                            <EmptyState message="Customer context will appear here when a conversation is selected." />
                        }
                    }
                >
                    <div class="spp-inbox__context-content">
                        <p>"Customer info + AI attributes"</p>
                    </div>
                </Show>
            </aside>
        </div>
    }
}
