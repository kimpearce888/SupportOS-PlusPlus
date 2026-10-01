//! Support Health page — operational facts (NOT a 0-100 score).
//!
//! Per spec §58: no aggregate health score. This page shows individual
//! operational metrics that let the user assess health themselves.
//!
//! Calls `support_health` IPC command on mount.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Support Health page.
#[component]
pub fn SupportHealthPage() -> impl IntoView {
    let health = create_rw_signal(serde_json::json!({}));
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let health = health;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "days_back": 7 });
            match crate::ipc::invoke::<serde_json::Value>("support_health", &args).await {
                Ok(data) => {
                    health.set(data);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-page spp-page--support-health">
            <h2 class="spp-page__title">"Support Health"</h2>

            <p class="spp-page__intro">
                "Operational facts — no aggregate score (per spec §58). Each metric is shown individually so you can assess health yourself."
            </p>

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
                {move || {
                    let h = health.get();
                    // The exact fields depend on spp_core::reports::get_health_facts.
                    // We render all key/value pairs from the JSON object.
                    let obj = h.as_object().cloned().unwrap_or_default();
                    if obj.is_empty() {
                        view! {
                            <EmptyState message="No health facts available for the selected time range." />
                        }.into_view()
                    } else {
                        view! {
                            <dl class="spp-health-facts">
                                {obj.iter().map(|(key, value)| {
                                    let val_str = match value {
                                        serde_json::Value::Number(n) => n.to_string(),
                                        serde_json::Value::String(s) => s.clone(),
                                        serde_json::Value::Bool(b) => b.to_string(),
                                        serde_json::Value::Null => "—".to_string(),
                                        other => other.to_string(),
                                    };
                                    view! {
                                        <div class="spp-health-fact">
                                            <dt>{key.clone()}</dt>
                                            <dd>{val_str}</dd>
                                        </div>
                                    }
                                }).collect::<Vec<_>>()}
                            </dl>
                        }.into_view()
                    }
                }.into_view()
                }
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Support Health page's UI rendering is verified by the wasm test runner in CI.
}
