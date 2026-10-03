//! Organizations pages — `/organizations` and `/organizations/:id`.
//!
//! Minimal reference-parity pages (`pages/Organizations.tsx`): a searchable
//! list fed by `GET /api/organizations`, rows linking to the org detail.
//! The port API derives organizations from the `customers.organization`
//! column (no separate table yet), so domains/customer/conversation counts
//! render as em-dashes and the detail endpoint currently answers 404 — the
//! detail page shows an honest error state until the backend grows a real
//! organizations table.
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.

use leptos::*;
use leptos_router::A;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Organizations list page — `/organizations`.
#[component]
pub fn OrganizationsPage() -> impl IntoView {
    let organizations = create_rw_signal(Vec::<serde_json::Value>::new());
    let total = create_rw_signal(0i64);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    let query = create_rw_signal(String::new());

    create_effect(move |_| {
        let organizations = organizations;
        let total = total;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/organizations?limit=50").await {
                Ok(v) => {
                    let items = v
                        .get("organizations")
                        .and_then(|o| o.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let t = v.get("total").and_then(|t| t.as_i64()).unwrap_or(0);
                    organizations.set(items);
                    total.set(t);
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
        <div class="spp-page spp-page--organizations">
            <header class="spp-page__header">
                <div>
                    <h2 class="spp-page__title">"Organizations"</h2>
                    <p class="spp-page__subtitle">
                        {move || format!("{} organizations", total.get())}
                    </p>
                </div>
                <input
                    class="spp-input"
                    type="text"
                    placeholder="Search organizations…"
                    aria-label="Search organizations"
                    prop:value=query
                    on:input=move |ev| query.set(event_target_value(&ev))
                />
            </header>

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
                    when=move || !filtered_organizations(organizations, query).is_empty()
                    fallback=move || {
                        view! {
                            <EmptyState message="No organizations found. They appear once synced customers carry an organization." />
                        }
                    }
                >
                    <table class="spp-table">
                        <thead>
                            <tr>
                                <th>"Organization"</th>
                                <th>"Domains"</th>
                                <th>"Customers"</th>
                                <th>"Conversations"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {move || {
                                filtered_organizations(organizations, query)
                                    .into_iter()
                                    .map(|org| {
                                        let id = org.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                        let name = org
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("(unnamed)")
                                            .to_string();
                                        let domains = org
                                            .get("domains")
                                            .and_then(|v| v.as_array())
                                            .map(|d| {
                                                d.iter()
                                                    .filter_map(|x| x.as_str())
                                                    .collect::<Vec<_>>()
                                                    .join(", ")
                                            })
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or_else(|| "—".to_string());
                                        let customers = org
                                            .get("customer_count")
                                            .and_then(|v| v.as_i64())
                                            .map(|c| c.to_string())
                                            .unwrap_or_else(|| "—".to_string());
                                        let conversations = org
                                            .get("conversation_count")
                                            .and_then(|v| v.as_i64())
                                            .map(|c| c.to_string())
                                            .unwrap_or_else(|| "—".to_string());
                                        let href = format!("/organizations/{id}");
                                        view! {
                                            <tr class="spp-table__row--clickable">
                                                <td>
                                                    <A href=href class="spp-table__link">
                                                        {name}
                                                    </A>
                                                </td>
                                                <td class="spp-table__cell-muted">{domains}</td>
                                                <td>
                                                    <span class="spp-badge">{customers}</span>
                                                </td>
                                                <td>
                                                    <span class="spp-badge">{conversations}</span>
                                                </td>
                                            </tr>
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            }}
                        </tbody>
                    </table>
                </Show>
            </Show>
        </div>
    }
}

/// The Organizations detail page — `/organizations/:id`.
#[component]
pub fn OrganizationDetailPage(organization_id: i64) -> impl IntoView {
    let organization = create_rw_signal(None::<serde_json::Value>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let organization = organization;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let path = format!("/api/organizations/{organization_id}");
            match crate::api::get_json::<serde_json::Value>(&path).await {
                Ok(v) => {
                    organization.set(Some(v));
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
        <div class="spp-page spp-page--organization-detail">
            <header class="spp-page__header">
                <h2 class="spp-page__title">"Organization"</h2>
                <A href="/organizations" class="spp-button spp-button--ghost spp-button--small">
                    "← All organizations"
                </A>
            </header>

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
                    when=move || organization.get().is_some()
                    fallback=move || {
                        view! {
                            <EmptyState message="Organization not found. It may have been deleted or never synced." />
                        }
                    }
                >
                    {move || {
                        let org = organization.get().unwrap_or_default();
                        let name = org
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(unnamed)")
                            .to_string();
                        view! {
                            <div class="spp-card">
                                <h3 class="spp-card__title">{name}</h3>
                                <p class="spp-page__subtitle">
                                    "Linked customers and their conversations appear here."
                                </p>
                            </div>
                        }
                    }}
                </Show>
            </Show>
        </div>
    }
}

/// Client-side name filter (the port API does not consume a `q` param yet).
fn filtered_organizations(
    organizations: leptos::RwSignal<Vec<serde_json::Value>>,
    query: leptos::RwSignal<String>,
) -> Vec<serde_json::Value> {
    let needle = query.get().to_lowercase();
    organizations
        .get()
        .into_iter()
        .filter(|org| {
            let name = org
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase();
            needle.is_empty() || name.contains(&needle)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_names_case_insensitively() {
        let _runtime = create_runtime();
        let organizations = create_rw_signal(vec![
            serde_json::json!({"id": 1, "name": "Acme Corp"}),
            serde_json::json!({"id": 2, "name": "Globex"}),
        ]);
        let query = create_rw_signal(String::new());
        assert_eq!(filtered_organizations(organizations, query).len(), 2);

        let query = create_rw_signal("acme".to_string());
        assert_eq!(filtered_organizations(organizations, query).len(), 1);

        let query = create_rw_signal("ACME".to_string());
        assert_eq!(filtered_organizations(organizations, query).len(), 1);

        let query = create_rw_signal("zilch".to_string());
        assert!(filtered_organizations(organizations, query).is_empty());
    }
}
