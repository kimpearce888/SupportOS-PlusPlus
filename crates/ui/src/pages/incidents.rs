//! Incidents page — list + filter by status.
//!
//! Per spec M7: "Incidents." Per A11: visual reference is the
//! reference repo's incidents screenshot.
//! Calls `incidents_list` IPC command on mount + on status filter change.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Incidents page.
#[component]
pub fn IncidentsPage() -> impl IntoView {
    let incidents = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    let status_filter = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let incidents = incidents;
        let loading = loading;
        let error_msg = error_msg;
        let current_status = status_filter.get();
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/incidents").await {
                Ok(data) => {
                    let items = data
                        .get("incidents")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    // The HTTP list returns all incidents; apply the status
                    // filter client-side (None = all).
                    let items = match &current_status {
                        Some(want) => items
                            .into_iter()
                            .filter(|i| {
                                i.get("status").and_then(|v| v.as_str()) == Some(want.as_str())
                            })
                            .collect(),
                        None => items,
                    };
                    incidents.set(items);
                    loading.set(false);
                    error_msg.set(None);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-page spp-page--incidents">
            <h2 class="spp-page__title">"Incidents"</h2>

            <p class="spp-page__intro">
                "Active and resolved incidents. Each incident is linked to a known issue and tracks severity + source."
            </p>

            <div class="spp-incidents__filters">
                <select
                    class="spp-incidents__filter"
                    value=move || status_filter.get().unwrap_or_default()
                    on:change=move |ev| {
                        let val = event_target_value(&ev);
                        let val = if val.is_empty() { None } else { Some(val) };
                        status_filter.set(val);
                        loading.set(true);
                    }
                >
                    <option value="">"All statuses"</option>
                    <option value="investigating">"Investigating"</option>
                    <option value="identified">"Identified"</option>
                    <option value="fix_in_progress">"Fix in progress"</option>
                    <option value="monitoring">"Monitoring"</option>
                    <option value="resolved">"Resolved"</option>
                </select>
            </div>

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
                    when=move || !incidents.with(|i| i.is_empty())
                    fallback=|| {
                        view! {
                            <EmptyState message="No incidents match the current filter." />
                        }
                    }
                >
                    <table class="spp-incidents__table">
                        <thead>
                            <tr>
                                <th>"ID"</th>
                                <th>"Status"</th>
                                <th>"Severity"</th>
                                <th>"Source"</th>
                                <th>"Description"</th>
                                <th>"Created"</th>
                                <th>"Updated"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {move || incidents.with(|items| {
                                items.iter().map(|i| {
                                    let id = i.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                    let status = i.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let severity = i.get("severity").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let source = i.get("source").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let description = i.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let created = i.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let updated = i.get("updated_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    view! {
                                        <tr>
                                            <td>{id.to_string()}</td>
                                            <td><span class="spp-badge spp-badge--status">{status}</span></td>
                                            <td>{severity}</td>
                                            <td>{source}</td>
                                            <td>{description}</td>
                                            <td>{created}</td>
                                            <td>{updated}</td>
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
    // The Incidents page's UI rendering is verified by the wasm test runner in CI.
}
