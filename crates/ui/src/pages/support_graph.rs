//! Support graph page — list nodes + view neighbors.
//!
//! Per spec M8: "support graph." Calls `graph_nodes_list` + `graph_neighbors`.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Support Graph page.
#[component]
pub fn SupportGraphPage() -> impl IntoView {
    let nodes = create_rw_signal(Vec::<serde_json::Value>::new());
    let selected_node = create_rw_signal(None::<(String, i64)>);
    let neighbors = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let nodes = nodes;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            // GET /api/graph/search (empty query lists the first nodes, LIKE
            // the reference GraphExplorer's search endpoint).
            match crate::api::get_json::<serde_json::Value>("/api/graph/search?q=").await {
                Ok(data) => {
                    let items = data
                        .get("results")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    nodes.set(items);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    create_effect(move |_| {
        let neighbors = neighbors;
        let error_msg = error_msg;
        if let Some((kind, nid)) = selected_node.get() {
            wasm_bindgen_futures::spawn_local(async move {
                let path = format!("/api/graph/neighbors/{kind}/{nid}");
                match crate::api::get_json::<serde_json::Value>(&path).await {
                    Ok(data) => {
                        neighbors.set(
                            data.get("neighbors")
                                .and_then(|v| v.as_array())
                                .cloned()
                                .unwrap_or_default(),
                        );
                    }
                    Err(e) => {
                        error_msg.set(Some(e));
                    }
                }
            });
        } else {
            neighbors.set(Vec::new());
        }
    });

    view! {
        <div class="spp-page spp-page--support-graph">
            <h2 class="spp-page__title">"Support Graph"</h2>

            <p class="spp-page__intro">
                "Graph of support entities (customers, conversations, issues, incidents) and their relationships. Click a node to see its neighbors."
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
                <div class="spp-graph__layout">
                    <aside class="spp-graph__nodes">
                        <h3>"Nodes"</h3>
                        <Show
                            when=move || !nodes.with(|n| n.is_empty())
                            fallback=|| {
                                view! {
                                    <EmptyState message="No graph nodes. The graph is built as conversations, issues, and incidents are linked." />
                                }
                            }
                        >
                            <ul class="spp-graph__node-list">
                                {move || nodes.with(|items| {
                                    items.iter().map(|n| {
                                        let id = n.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                        let kind = n.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let label = n.get("label").and_then(|v| v.as_str()).unwrap_or("(no label)").to_string();
                                        let kind_for_click = kind.clone();
                                        let kind_for_badge = kind.clone();
                                        let is_selected =
                                            move || selected_node.get() == Some((kind.clone(), id));
                                        view! {
                                            <li
                                                class="spp-graph__node"
                                                class:is-selected=is_selected
                                                on:click=move |_| {
                                                    selected_node.set(Some((kind_for_click.clone(), id)));
                                                }
                                            >
                                                <span class="spp-badge spp-badge--status">{kind_for_badge}</span>
                                                <span class="spp-graph__node-label">{label}</span>
                                                <span class="spp-graph__node-id">{"#"}{id.to_string()}</span>
                                            </li>
                                        }
                                    }).collect::<Vec<_>>()
                                })}
                            </ul>
                        </Show>
                    </aside>

                    <main class="spp-graph__neighbors">
                        <Show
                            when=move || selected_node.get().is_some()
                            fallback=|| {
                                view! {
                                    <EmptyState message="Select a node to view its neighbors." />
                                }
                            }
                        >
                            <Show
                                when=move || !neighbors.with(|n| n.is_empty())
                                fallback=|| {
                                    view! {
                                        <EmptyState message="This node has no neighbors (no edges connected)." />
                                    }
                                }
                            >
                                <h3>"Neighbors"</h3>
                                <ul class="spp-graph__neighbor-list">
                                    {move || neighbors.with(|items| {
                                        items.iter().map(|n| {
                                            let id = n.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                            let kind = n.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let label = n.get("label").and_then(|v| v.as_str()).unwrap_or("(no label)").to_string();
                                            view! {
                                                <li class="spp-graph__neighbor">
                                                    <span class="spp-badge">{kind}</span>
                                                    <span class="spp-graph__neighbor-label">{label}</span>
                                                    <span class="spp-graph__neighbor-id">{"#"}{id.to_string()}</span>
                                                </li>
                                            }
                                        }).collect::<Vec<_>>()
                                    })}
                                </ul>
                            </Show>
                        </Show>
                    </main>
                </div>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Support Graph page's UI rendering is verified by the wasm test runner in CI.
}
