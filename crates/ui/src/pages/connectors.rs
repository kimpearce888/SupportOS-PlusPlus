//! Connectors page — list configured data connectors.
//!
//! Per spec M10: "connectors with SSRF guard." Calls `connectors_list` IPC.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Connectors page.
#[component]
pub fn ConnectorsPage() -> impl IntoView {
    let connectors = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let connectors = connectors;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/connectors").await {
                Ok(data) => {
                    let items = data
                        .get("connectors")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    connectors.set(items);
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
        <div class="spp-page spp-page--connectors">
            <h2 class="spp-page__title">"Connectors"</h2>

            <p class="spp-page__intro">
                "External data sources (local JSON, CSV, SQLite, HTTP). Each connector has an SSRF guard that blocks private IP ranges."
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
                <Show
                    when=move || !connectors.with(|c| c.is_empty())
                    fallback=|| {
                        view! {
                            <EmptyState message="No connectors configured. Use the data tools CLI or add them via the database directly." />
                        }
                    }
                >
                    <table class="spp-connectors__table">
                        <thead>
                            <tr>
                                <th>"ID"</th>
                                <th>"Name"</th>
                                <th>"Kind"</th>
                                <th>"Auth mode"</th>
                                <th>"Created"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {move || connectors.with(|items| {
                                items.iter().map(|c| {
                                    let id = c.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let kind = c.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let auth = c.get("auth_mode").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let created = c.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    view! {
                                        <tr>
                                            <td>{id.to_string()}</td>
                                            <td>{name}</td>
                                            <td><span class="spp-badge">{kind}</span></td>
                                            <td>{auth}</td>
                                            <td>{created}</td>
                                        </tr>
                                    }
                                }).collect::<Vec<_>>()
                            })}
                        </tbody>
                    </table>
                </Show>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Connectors page's UI rendering is verified by the wasm test runner in CI.
}
