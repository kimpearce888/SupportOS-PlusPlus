//! Command palette page — full-page version of the quick-action search.
//!
//! Per spec M3: "command palette." This page shows the same actions as the
//! Cmd/Ctrl+K palette but as a full page (for discoverability + accessibility).
//! The keyboard-triggered overlay lives in `components/command_palette.rs`.

use leptos::*;

/// The Command Palette page.
#[component]
pub fn CommandPalettePage() -> impl IntoView {
    let query = create_rw_signal(String::new());

    // The list of available actions (same as the overlay palette).
    let actions = vec![
        ("Go to Dashboard", "Navigate to the dashboard"),
        ("Go to Inbox", "View conversations"),
        ("Go to Sync Health", "Check sync status"),
        ("Go to Operations Center", "View operations tiles"),
        ("Go to Notifications", "View notification center"),
        ("Go to Automation", "View automation rules + approvals"),
        ("Go to Settings", "Configure the app"),
        ("Go to AI Center", "Configure AI provider + models"),
        ("Go to Reports", "Build a custom report"),
        (
            "Go to Issue Radar",
            "View known issues + clusters + incidents",
        ),
        ("Go to Search", "Search conversations + customers"),
        ("Go to Customers", "Search customers"),
        ("Go to Support Health", "View operational health facts"),
        ("Go to Backup", "Export data as JSON"),
    ];

    let filtered_actions = create_memo(move |_| {
        let q = query.get().to_lowercase();
        if q.is_empty() {
            actions.clone()
        } else {
            actions
                .iter()
                .filter(|(label, desc)| {
                    label.to_lowercase().contains(&q) || desc.to_lowercase().contains(&q)
                })
                .cloned()
                .collect::<Vec<_>>()
        }
    });

    view! {
        <div class="spp-page spp-page--command-palette">
            <h2 class="spp-page__title">"Command Palette"</h2>

            <p class="spp-page__intro">
                "Quick navigation + actions. Press Cmd/Ctrl+K anywhere in the app to open the palette overlay, or use this page directly."
            </p>

            <div class="spp-command-palette__search">
                <input
                    class="spp-command-palette__input"
                    type="text"
                    placeholder="Type to filter actions..."
                    prop:value=query
                    on:input=move |ev| query.set(event_target_value(&ev))
                    autofocus=true
                />
            </div>

            <ul class="spp-command-palette__list">
                {move || {
                    filtered_actions.get()
                        .into_iter()
                        .map(|(label, desc)| {
                            view! {
                                <li class="spp-command-palette__item">
                                    <span class="spp-command-palette__label">{label}</span>
                                    <span class="spp-command-palette__desc">{desc}</span>
                                </li>
                            }
                        })
                        .collect::<Vec<_>>()
                }}
            </ul>

            <Show when=move || filtered_actions.with(|a| a.is_empty()) fallback=|| ()>
                <div class="spp-state">
                    <p class="spp-state__body">"No actions match your query. Try a different search."</p>
                </div>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Command Palette page's UI rendering is verified by the wasm test runner in CI.
}
