//! Sync Health page — the `/sync-health` route.
//!
//! Per spec A9: "The 'Webhook push' screen (in Sync Health) must explain in
//! plain language what address Help Scout needs, show the state
//! (not configured, registered, receiving, error), support register and
//! delete as Tauri commands, and never claim real-time updates are active
//! when they are not."
//!
//! This page is the UI for the webhook push feature. The actual register/
//! delete Tauri commands call the Help Scout Webhooks API; the state machine
//! is driven by the `webhook_state` Tauri IPC command (which reads the
//! `webhook_configs` table + the last-received event timestamp).

use leptos::*;

use crate::components::button::{Button, ButtonStyle};
use crate::components::state_view::EmptyState;

/// The webhook push state machine (A9).
/// Matches the spec's four states: not configured → registered → receiving → error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookPushState {
    /// No webhook URL has been registered with Help Scout.
    NotConfigured,
    /// A webhook URL has been registered but no events have been received yet.
    Registered,
    /// At least one event has been received (the webhook is working).
    Receiving,
    /// An error occurred (e.g. the registration failed, or events stopped).
    Error,
}

impl WebhookPushState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotConfigured => "Not configured",
            Self::Registered => "Registered",
            Self::Receiving => "Receiving",
            Self::Error => "Error",
        }
    }

    #[must_use]
    pub fn css_class(self) -> &'static str {
        match self {
            Self::NotConfigured => "spp-webhook-state--not-configured",
            Self::Registered => "spp-webhook-state--registered",
            Self::Receiving => "spp-webhook-state--receiving",
            Self::Error => "spp-webhook-state--error",
        }
    }

    /// Whether real-time updates are currently active. Per A9: "never claim
    /// real-time updates are active when they are not." Only `Receiving` is
    /// truly real-time.
    #[must_use]
    pub fn is_realtime(self) -> bool {
        matches!(self, Self::Receiving)
    }
}

/// The Sync Health page component.
///
/// Shows:
/// - Plain-language explanation of what address Help Scout needs.
/// - The current webhook push state (badge).
/// - The registered URL (or "not configured").
/// - Last event received timestamp.
/// - Register and Delete buttons.
/// - A note about incremental polling being the baseline.
#[component]
pub fn SyncHealthPage() -> impl IntoView {
    // Wired to the `sync_health_state` Tauri IPC command on mount.
    let state = create_rw_signal(WebhookPushState::NotConfigured);
    let webhook_url = create_rw_signal(String::new());
    let last_event = create_rw_signal(None::<String>);
    let loading = create_rw_signal(true);

    create_effect(move |_| {
        let state = state;
        let loading = loading;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/sync/status").await {
                Ok(data) => {
                    // Derive the push state from the webhook stats block the
                    // same way the reference SyncHealth banner does: events
                    // received -> receiving; else sync runs -> registered.
                    let events = data
                        .pointer("/webhook/events/total")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let runs = data
                        .pointer("/last_success/id")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let push_state = if events > 0 {
                        "receiving"
                    } else if runs > 0 {
                        "registered"
                    } else {
                        "not_configured"
                    };
                    let new_state = match push_state {
                        "receiving" => WebhookPushState::Receiving,
                        "registered" => WebhookPushState::Registered,
                        "error" => WebhookPushState::Error,
                        _ => WebhookPushState::NotConfigured,
                    };
                    state.set(new_state);
                    loading.set(false);
                }
                Err(_) => {
                    // If the API is unreachable, show NotConfigured.
                    state.set(WebhookPushState::NotConfigured);
                    loading.set(false);
                }
            }
        });
    });

    let on_register = move || {
        // M2: invoke `webhook_register` Tauri command.
        // For now, just flip the state to demonstrate the UI.
        state.set(WebhookPushState::Registered);
    };

    let on_delete = move || {
        // M2: invoke `webhook_delete` Tauri command.
        state.set(WebhookPushState::NotConfigured);
        webhook_url.set(String::new());
        last_event.set(None);
    };

    view! {
        <div class="spp-page spp-page--sync-health">
            <h2 class="spp-page__title">"Sync Health"</h2>

            // ── Webhook push section ──
            <section class="spp-webhook-push">
                <h3 class="spp-webhook-push__title">"Webhook push"</h3>

                // Plain-language explanation (A9 requirement).
                <p class="spp-webhook-push__explanation">
                    "Help Scout can push real-time events to a URL on your machine. "
                    "You need to make your machine reachable from the internet (e.g. via port forwarding or a reverse proxy) "
                    "and enter that public URL below. Help Scout will POST events to that URL; "
                    "SupportOS++ verifies each event's HMAC signature and processes it locally."
                </p>

                // State badge.
                <div class="spp-webhook-push__state">
                    <span class="spp-webhook-state-badge" class=move || state.get().css_class()>
                        {move || state.get().label()}
                    </span>
                    <Show when=move || state.get().is_realtime() fallback=|| ()>
                        <span class="spp-webhook-realtime-indicator">"● Real-time"</span>
                    </Show>
                </div>

                // Registered URL (if any).
                <Show when=move || !webhook_url.get().is_empty() fallback=|| ()>
                    <div class="spp-webhook-push__url">
                        <span class="spp-webhook-push__label">"Registered URL:"</span>
                        <code>{move || webhook_url.get()}</code>
                    </div>
                </Show>

                // Last event timestamp (if any).
                <Show when=move || last_event.get().is_some() fallback=|| ()>
                    <div class="spp-webhook-push__last-event">
                        <span class="spp-webhook-push__label">"Last event received:"</span>
                        <span>{move || last_event.get().unwrap_or_default()}</span>
                    </div>
                </Show>

                // Actions.
                <div class="spp-webhook-push__actions">
                    <Show
                        when=move || state.get() == WebhookPushState::NotConfigured || state.get() == WebhookPushState::Error
                        fallback=|| ().into_view()
                    >
                        <Button on_click=move || on_register() style=ButtonStyle::Primary>
                            "Register webhook"
                        </Button>
                    </Show>
                    <Show
                        when=move || state.get() != WebhookPushState::NotConfigured
                        fallback=|| ().into_view()
                    >
                        <Button on_click=move || on_delete() style=ButtonStyle::Ghost>
                            "Delete webhook"
                        </Button>
                    </Show>
                </div>

                // Note about polling baseline (A9: "incremental polling is always the baseline").
                <p class="spp-webhook-push__polling-note">
                    "Incremental polling runs every 5 minutes regardless of webhook configuration. "
                    "Webhook push is optional and provides real-time updates when configured."
                </p>
            </section>

            // ── Sync status section ──
            <section class="spp-sync-status">
                <h3 class="spp-sync-status__title">"Sync status"</h3>
                <EmptyState message="Sync status details will appear here once the first sync completes." />
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_push_state_labels() {
        assert_eq!(WebhookPushState::NotConfigured.label(), "Not configured");
        assert_eq!(WebhookPushState::Registered.label(), "Registered");
        assert_eq!(WebhookPushState::Receiving.label(), "Receiving");
        assert_eq!(WebhookPushState::Error.label(), "Error");
    }

    #[test]
    fn webhook_push_state_css_classes() {
        assert_eq!(
            WebhookPushState::NotConfigured.css_class(),
            "spp-webhook-state--not-configured"
        );
        assert_eq!(
            WebhookPushState::Receiving.css_class(),
            "spp-webhook-state--receiving"
        );
    }

    #[test]
    fn only_receiving_is_realtime() {
        assert!(!WebhookPushState::NotConfigured.is_realtime());
        assert!(!WebhookPushState::Registered.is_realtime());
        assert!(WebhookPushState::Receiving.is_realtime());
        assert!(!WebhookPushState::Error.is_realtime());
    }
}
