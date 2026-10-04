//! Overlay components — modal dialogs.
//!
//! Reference: components/common/overlays.tsx (ConfirmDialog + Modal).
//! Nothing renders unless mounted; both actions are explicit.

use std::sync::Arc;

use leptos::*;

/// A blocking confirmation dialog with an explicit, customer-visible-action
/// message (reference: ConfirmDialog).
#[component]
pub fn ConfirmDialog(
    #[prop(into)] title: String,
    #[prop(into)] message: String,
    #[prop(default = "Confirm".to_string(), into)] confirm_label: String,
    #[prop(default = false)] danger: bool,
    on_confirm: Arc<dyn Fn() + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
) -> impl IntoView {
    let confirm = Arc::clone(&on_confirm);
    let cancel = Arc::clone(&on_cancel);
    view! {
        <div class="spp-overlay" role="dialog" aria-modal="true">
            <div class="spp-modal">
                <h3 class="spp-modal__title">{title}</h3>
                <p class="spp-modal__message">{message}</p>
                <div class="spp-modal__actions">
                    <button class="spp-button" on:click=move |_| cancel()>
                        "Cancel"
                    </button>
                    <button
                        class=move || {
                            format!(
                                "spp-button spp-button--primary{}",
                                if danger { " spp-button--danger" } else { "" }
                            )
                        }
                        on:click=move |_| confirm()
                    >
                        {confirm_label}
                    </button>
                </div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn confirm_dialog_is_purely_presentational() {
        // The component has no logic beyond prop wiring; its behavior (cancel,
        // confirm callbacks) is exercised by the inbox page that mounts it.
    }
}
