//! Issue Radar page — known issues + clusters + incidents overview.
//!
//! Per spec M7: "Issue Radar." Per A11: visual reference is the
//! reference repo's issue radar screenshot.
//! Per KNOWN PITFALLS: "every view has loading, empty and error states."
//!
//! Calls `issue_radar_snapshot` IPC command on mount.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Issue Radar page.
#[component]
pub fn IssueRadarPage() -> impl IntoView {
    let snapshot = create_rw_signal(serde_json::json!({}));
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let snapshot = snapshot;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({});
            match crate::ipc::invoke::<serde_json::Value>("issue_radar_snapshot", &args).await {
                Ok(data) => {
                    snapshot.set(data);
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
        <div class="spp-page spp-page--issue-radar">
            <h2 class="spp-page__title">"Issue Radar"</h2>

            <p class="spp-page__intro">
                "Active known issues, issue clusters, and incidents. The radar surfaces emerging problems before they escalate."
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
                    let s = snapshot.get();
                    let active_known_issues = s.get("active_known_issues").and_then(|v| v.as_u64()).unwrap_or(0);
                    let active_clusters = s.get("active_clusters").and_then(|v| v.as_u64()).unwrap_or(0);
                    let active_incidents = s.get("active_incidents").and_then(|v| v.as_u64()).unwrap_or(0);

                    view! {
                        <div class="spp-radar-grid">
                            <div class="spp-radar-card">
                                <span class="spp-radar-card__label">"Active known issues"</span>
                                <span class="spp-radar-card__value">{active_known_issues.to_string()}</span>
                                <span class="spp-radar-card__hint">"Documented issues currently affecting customers"</span>
                            </div>
                            <div class="spp-radar-card">
                                <span class="spp-radar-card__label">"Active clusters"</span>
                                <span class="spp-radar-card__value">{active_clusters.to_string()}</span>
                                <span class="spp-radar-card__hint">"Groups of similar issues identified by clustering"</span>
                            </div>
                            <div class="spp-radar-card spp-radar-card--incident">
                                <span class="spp-radar-card__label">"Active incidents"</span>
                                <span class="spp-radar-card__value">{active_incidents.to_string()}</span>
                                <span class="spp-radar-card__hint">"Unresolved incidents (status != resolved)"</span>
                            </div>
                        </div>

                        {if active_known_issues == 0 && active_clusters == 0 && active_incidents == 0 {
                            view! {
                                <EmptyState message="No active issues, clusters, or incidents. The radar is clear." />
                            }.into_view()
                        } else {
                            ().into_view()
                        }}
                    }.into_view()
                }}
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Issue Radar page's UI rendering is verified by the wasm test runner in CI.
}
