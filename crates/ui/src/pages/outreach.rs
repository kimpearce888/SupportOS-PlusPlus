//! Outreach page — segments + campaigns + do-not-contact.
//!
//! Per spec M9: "segmentation, saved segments, campaigns, do-not-contact,
//! monitoring." Calls `segments_list`, `campaigns_list`, `dnc_list` IPCs.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Outreach page.
#[component]
pub fn OutreachPage() -> impl IntoView {
    let segments = create_rw_signal(Vec::<serde_json::Value>::new());
    let campaigns = create_rw_signal(Vec::<serde_json::Value>::new());
    let dnc = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let segments = segments;
        let campaigns = campaigns;
        let dnc = dnc;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let seg_result = crate::ipc::invoke::<Vec<serde_json::Value>>(
                "segments_list",
                &serde_json::json!({}),
            )
            .await;
            let camp_result = crate::ipc::invoke::<Vec<serde_json::Value>>(
                "campaigns_list",
                &serde_json::json!({}),
            )
            .await;
            let dnc_result =
                crate::ipc::invoke::<Vec<serde_json::Value>>("dnc_list", &serde_json::json!({}))
                    .await;
            match (seg_result, camp_result, dnc_result) {
                (Ok(s), Ok(c), Ok(d)) => {
                    segments.set(s);
                    campaigns.set(c);
                    dnc.set(d);
                    loading.set(false);
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-page spp-page--outreach">
            <h2 class="spp-page__title">"Outreach"</h2>

            <p class="spp-page__intro">
                "Saved segments, campaigns with livelock prevention, and the do-not-contact list."
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
                <section class="spp-outreach__section">
                    <h3>"Campaigns"</h3>
                    <Show
                        when=move || !campaigns.with(|c| c.is_empty())
                        fallback=|| {
                            view! {
                                <EmptyState message="No campaigns defined." />
                            }
                        }
                    >
                        <table class="spp-outreach__table">
                            <thead>
                                <tr>
                                    <th>"ID"</th>
                                    <th>"Name"</th>
                                    <th>"Status"</th>
                                    <th>"Segment"</th>
                                    <th>"Created"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {move || campaigns.with(|items| {
                                    items.iter().map(|c| {
                                        let id = c.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let segment = c.get("segment_id").and_then(|v| v.as_i64()).unwrap_or(0);
                                        let created = c.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        view! {
                                            <tr>
                                                <td>{id.to_string()}</td>
                                                <td>{name}</td>
                                                <td><span class="spp-badge">{status}</span></td>
                                                <td>{segment.to_string()}</td>
                                                <td>{created}</td>
                                            </tr>
                                        }
                                    }).collect::<Vec<_>>()
                                })}
                            </tbody>
                        </table>
                    </Show>
                </section>

                <section class="spp-outreach__section">
                    <h3>"Saved segments"</h3>
                    <Show
                        when=move || !segments.with(|s| s.is_empty())
                        fallback=|| {
                            view! {
                                <EmptyState message="No saved segments defined." />
                            }
                        }
                    >
                        <table class="spp-outreach__table">
                            <thead>
                                <tr>
                                    <th>"ID"</th>
                                    <th>"Name"</th>
                                    <th>"Criteria"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {move || segments.with(|items| {
                                    items.iter().map(|s| {
                                        let id = s.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let criteria = s.get("criteria").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        view! {
                                            <tr>
                                                <td>{id.to_string()}</td>
                                                <td>{name}</td>
                                                <td><code>{criteria}</code></td>
                                            </tr>
                                        }
                                    }).collect::<Vec<_>>()
                                })}
                            </tbody>
                        </table>
                    </Show>
                </section>

                <section class="spp-outreach__section">
                    <h3>"Do-not-contact list"</h3>
                    <Show
                        when=move || !dnc.with(|d| d.is_empty())
                        fallback=|| {
                            view! {
                                <EmptyState message="No customers on the do-not-contact list." />
                            }
                        }
                    >
                        <table class="spp-outreach__table">
                            <thead>
                                <tr>
                                    <th>"Customer ID"</th>
                                    <th>"Reason"</th>
                                    <th>"Added"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {move || dnc.with(|items| {
                                    items.iter().map(|d| {
                                        let cid = d.get("customer_id").and_then(|v| v.as_i64()).unwrap_or(0);
                                        let reason = d.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let added = d.get("added_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        view! {
                                            <tr>
                                                <td>{cid.to_string()}</td>
                                                <td>{reason}</td>
                                                <td>{added}</td>
                                            </tr>
                                        }
                                    }).collect::<Vec<_>>()
                                })}
                            </tbody>
                        </table>
                    </Show>
                </section>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Outreach page's UI rendering is verified by the wasm test runner in CI.
}
