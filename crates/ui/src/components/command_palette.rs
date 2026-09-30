//! Command palette (Cmd/Ctrl+K) — quick search + actions (M3-T07).
//!
//! Per spec M3: "command palette."
//! Per KNOWN PITFALLS:
//! - "Escape closes only the topmost dialog."
//! - "every view has loading, empty and error states."

use leptos::*;

/// A command palette action.
#[derive(Debug, Clone)]
pub struct PaletteAction {
    pub label: String,
    pub description: String,
}

/// The command palette component. Triggered by Cmd/Ctrl+K.
#[component]
pub fn CommandPalette<F>(#[prop(into)] open: MaybeSignal<bool>, on_close: F) -> impl IntoView
where
    F: Fn() + 'static,
{
    let query = create_rw_signal(String::new());

    // Store the close handler in a StoredValue so the view macro can use it
    // from `Fn` closures (not `FnOnce`). Same pattern as OnboardingOverlay.
    let close_handler = StoredValue::new(std::sync::Arc::new(on_close) as std::sync::Arc<dyn Fn()>);

    let default_actions = StoredValue::new(vec![
        PaletteAction {
            label: "Go to Dashboard".into(),
            description: "Navigate to the dashboard".into(),
        },
        PaletteAction {
            label: "Go to Inbox".into(),
            description: "View conversations".into(),
        },
        PaletteAction {
            label: "Go to Sync Health".into(),
            description: "Check sync status".into(),
        },
        PaletteAction {
            label: "Go to Settings".into(),
            description: "Configure SupportOS++".into(),
        },
    ]);

    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div
                class="spp-command-palette-overlay"
                role="dialog"
                on:click=move |_| {
                    close_handler.with_value(|f| f());
                }
            >
                <div class="spp-command-palette" on:click=move |e| e.stop_propagation()>
                    <div class="spp-command-palette__input">
                        <span class="spp-command-palette__icon">"⌘"</span>
                        <input
                            type="text"
                            class="spp-command-palette__field"
                            placeholder="Search or type a command…"
                            prop:value=move || query.get()
                            on:input=move |e| {
                                query.set(event_target_value(&e));
                            }
                            on:keydown=move |e| {
                                if e.key() == "Escape" {
                                    close_handler.with_value(|f| f());
                                }
                            }
                        />
                    </div>
                    <div class="spp-command-palette__results">
                        <div class="spp-command-palette__list">
                            {move || {
                                default_actions.with_value(|actions| {
                                    actions.iter().map(|action| {
                                        view! {
                                            <div class="spp-command-palette__item">
                                                <span class="spp-command-palette__item-label">
                                                    {action.label.clone()}
                                                </span>
                                                <span class="spp-command-palette__item-desc">
                                                    {action.description.clone()}
                                                </span>
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()
                                })
                            }}
                        </div>
                    </div>
                </div>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_action_construction() {
        let action = PaletteAction {
            label: "Go to Dashboard".into(),
            description: "Navigate to the dashboard".into(),
        };
        assert_eq!(action.label, "Go to Dashboard");
    }
}
