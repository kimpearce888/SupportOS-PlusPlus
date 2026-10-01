//! Knowledge gaps page — topics with the most missing docs.
//!
//! Per spec M7: "Knowledge docs." Calls `knowledge_gaps_list` IPC.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Knowledge Gaps page.
#[component]
pub fn KnowledgeGapsPage() -> impl IntoView {
    let gaps = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let gaps = gaps;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "limit": 20 });
            match crate::ipc::invoke::<Vec<serde_json::Value>>("knowledge_gaps_list", &args).await {
                Ok(items) => {
                    gaps.set(items);
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
        <div class="spp-page spp-page--knowledge-gaps">
            <h2 class="spp-page__title">"Knowledge Gaps"</h2>

            <p class="spp-page__intro">
                "Topics where customers ask questions that the knowledge base doesn't cover. Each gap shows the count of related unanswered conversations."
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
                    when=move || !gaps.with(|g| g.is_empty())
                    fallback=|| {
                        view! {
                            <EmptyState message="No knowledge gaps detected. The knowledge base covers all customer questions in the time range." />
                        }
                    }
                >
                    <table class="spp-knowledge-gaps__table">
                        <thead>
                            <tr>
                                <th>"Topic"</th>
                                <th>"Gap count"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {move || gaps.with(|items| {
                                items.iter().map(|g| {
                                    let topic = g.get("topic").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let count = g.get("gap_count").and_then(|v| v.as_i64()).unwrap_or(0);
                                    view! {
                                        <tr>
                                            <td>{topic}</td>
                                            <td>{count.to_string()}</td>
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
    // The Knowledge Gaps page's UI rendering is verified by the wasm test runner in CI.
}
