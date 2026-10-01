//! Search page — universal search across conversations + customers.
//!
//! Per spec M3: "lexical universal search." Calls `universal_search` IPC.
//! Uses FTS5 under the hood.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Search page.
#[component]
pub fn SearchPage() -> impl IntoView {
    let query = create_rw_signal(String::new());
    let results = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(false);
    let error_msg = create_rw_signal(None::<String>);
    let has_searched = create_rw_signal(false);

    let do_search = move || {
        let q = query.get();
        if q.trim().is_empty() {
            results.set(Vec::new());
            has_searched.set(false);
            return;
        }
        loading.set(true);
        error_msg.set(None);
        has_searched.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "query": q });
            match crate::ipc::invoke::<Vec<serde_json::Value>>("universal_search", &args).await {
                Ok(r) => {
                    results.set(r);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    };

    view! {
        <div class="spp-page spp-page--search">
            <h2 class="spp-page__title">"Search"</h2>

            <p class="spp-page__intro">
                "Universal search across conversations + customers (FTS5-powered)."
            </p>

            <div class="spp-search__bar">
                <input
                    class="spp-search__input"
                    type="text"
                    placeholder="Search conversations, customers..."
                    prop:value=query
                    on:input=move |ev| query.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            do_search();
                        }
                    }
                />
                <button class="spp-button" on:click=move |_| do_search()>
                    "Search"
                </button>
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
                when=move || !loading.get() && error_msg.get().is_none() && has_searched.get()
                fallback=|| ()
            >
                <Show
                    when=move || !results.with(|r| r.is_empty())
                    fallback=|| {
                        view! {
                            <EmptyState message="No results found. Try a different query." />
                        }
                    }
                >
                    <ul class="spp-search__results">
                        {move || results.with(|items| {
                            items.iter().map(|r| {
                                let resource_type = r.get("resource_type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let remote_id = r.get("remote_id").and_then(|v| v.as_i64()).unwrap_or(0);
                                let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let snippet = r.get("snippet").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                view! {
                                    <li class="spp-search__result">
                                        <div class="spp-search__result-header">
                                            <span class="spp-badge spp-badge--status">{resource_type}</span>
                                            <span class="spp-search__result-id">{"#"}{remote_id.to_string()}</span>
                                        </div>
                                        <div class="spp-search__result-title">{title}</div>
                                        <div class="spp-search__result-snippet">{snippet}</div>
                                    </li>
                                }
                            }).collect::<Vec<_>>()
                        })}
                    </ul>
                </Show>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Search page's UI rendering is verified by the wasm test runner in CI.
}
