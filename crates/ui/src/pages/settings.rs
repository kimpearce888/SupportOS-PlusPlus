//! Settings page — the `/settings` route.
//!
//! Reads the live application settings over the HTTP API (the same endpoints
//! the reference SettingsPage uses): `GET /api/settings`,
//! `GET /api/settings/lmstudio`, `GET /api/settings/qdrant`.
//! Per KNOWN PITFALLS: loading, empty, and error states everywhere.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Settings page.
#[component]
pub fn SettingsPage() -> impl IntoView {
    let settings = create_rw_signal(None::<serde_json::Value>);
    let lmstudio = create_rw_signal(None::<serde_json::Value>);
    let qdrant = create_rw_signal(None::<serde_json::Value>);
    let hs_configured = create_rw_signal(None::<bool>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let settings = settings;
        let lmstudio = lmstudio;
        let qdrant = qdrant;
        let hs_configured = hs_configured;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let s = crate::api::get_json::<serde_json::Value>("/api/settings").await;
            let l = crate::api::get_json::<serde_json::Value>("/api/settings/lmstudio").await;
            let q = crate::api::get_json::<serde_json::Value>("/api/settings/qdrant").await;
            // The onboarding payload carries the Help Scout connection state
            // (hs_configured), like the reference's connection banner.
            let o = crate::api::get_json::<serde_json::Value>("/api/onboarding").await;
            match (s, l, q, o) {
                (Ok(s), Ok(l), Ok(q), Ok(o)) => {
                    settings.set(Some(s));
                    lmstudio.set(Some(l));
                    qdrant.set(Some(q));
                    hs_configured.set(o.get("hs_configured").and_then(|v| v.as_bool()));
                    loading.set(false);
                }
                (Err(e), _, _, _) | (_, Err(e), _, _) | (_, _, Err(e), _) | (_, _, _, Err(e)) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-page spp-page--settings">
            <h2 class="spp-page__title">"Settings"</h2>

            <Show when=move || loading.get() fallback=|| ()>
                <LoadingState />
            </Show>

            <Show when=move || error_msg.get().is_some() fallback=|| ()>
                <div class="spp-state spp-state--error">
                    <span class="spp-state__icon" aria-hidden="true">"⚠"</span>
                    <p class="spp-state__body">{move || error_msg.get().unwrap_or_default()}</p>
                </div>
            </Show>

            <Show
                when=move || !loading.get() && error_msg.get().is_none()
                fallback=|| ()
            >
                <section class="spp-settings__section">
                    <h3>"Synchronization"</h3>
                    {move || {
                        let s = settings.get().unwrap_or_default();
                        let interval = s.get("sync_interval_minutes").and_then(|v| v.as_i64()).unwrap_or(5);
                        view! {
                            <div class="spp-settings__field">
                                <label>"Sync interval (minutes)"</label>
                                <span>{interval.to_string()}</span>
                            </div>
                        }.into_view()
                    }}
                </section>

                <section class="spp-settings__section">
                    <h3>"LM Studio (local AI gateway)"</h3>
                    {move || {
                        let l = lmstudio.get().unwrap_or_default();
                        let base = l.get("base_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let chat = l.get("chat_model").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let embed = l.get("embedding_model").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        if base.is_empty() && chat.is_empty() {
                            return view! {
                                <EmptyState message="No AI gateway configured. Keyword search and manual workflows remain fully functional." />
                            }.into_view();
                        }
                        view! {
                            <div class="spp-settings__field">
                                <label>"Base URL"</label>
                                <span class="mono">{base}</span>
                            </div>
                            <div class="spp-settings__field">
                                <label>"Chat model"</label>
                                <span>{chat}</span>
                            </div>
                            <div class="spp-settings__field">
                                <label>"Embedding model"</label>
                                <span>{embed}</span>
                            </div>
                        }.into_view()
                    }}
                </section>

                <section class="spp-settings__section">
                    <h3>"Qdrant (local vector store, optional)"</h3>
                    {move || {
                        let q = qdrant.get().unwrap_or_default();
                        let enabled = q.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                        let url = q.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        view! {
                            <div class="spp-settings__field">
                                <label>"Enabled"</label>
                                <span class="spp-badge">
                                    {if enabled { "✅ enabled" } else { "❌ disabled" }}
                                </span>
                            </div>
                            <div class="spp-settings__field">
                                <label>"URL"</label>
                                <span class="mono">{url}</span>
                            </div>
                            <p class="spp-settings__hint">
                                "Keyword search (SQLite FTS5) remains fully functional without it."
                            </p>
                        }.into_view()
                    }}
                </section>

                <section class="spp-settings__section">
                    <h3>"Help Scout connection"</h3>
                    {move || {
                        match hs_configured.get() {
                            Some(true) => view! {
                                <div class="spp-settings__field">
                                    <label>"OAuth credentials"</label>
                                    <span class="spp-badge">"✅ configured"</span>
                                </div>
                            }.into_view(),
                            Some(false) => view! {
                                <EmptyState message="Help Scout is not connected. Run the connection flow from the Sync Health page." />
                            }.into_view(),
                            None => view! {
                                <EmptyState message="Help Scout connection state is unavailable." />
                            }.into_view(),
                        }
                    }}
                </section>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Settings page's UI rendering is verified by the wasm test runner in CI.
}
