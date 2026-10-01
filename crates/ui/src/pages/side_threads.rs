//! Side threads page — list threads + messages for a conversation.
//!
//! Per spec M4: "side threads, mentions." Calls `side_threads_list` +
//! `side_thread_messages` IPC commands.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Side Threads page.
#[component]
pub fn SideThreadsPage(conversation_id: i64) -> impl IntoView {
    let threads = create_rw_signal(Vec::<serde_json::Value>::new());
    let selected_thread_id = create_rw_signal(None::<i64>);
    let messages = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    // Load threads on mount + when conversation_id changes.
    create_effect(move |_| {
        let threads = threads;
        let loading = loading;
        let error_msg = error_msg;
        let cid = conversation_id;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "conversation_id": cid });
            match crate::ipc::invoke::<Vec<serde_json::Value>>("side_threads_list", &args).await {
                Ok(items) => {
                    threads.set(items);
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

    // Load messages when a thread is selected.
    create_effect(move |_| {
        let messages = messages;
        let error_msg = error_msg;
        if let Some(tid) = selected_thread_id.get() {
            wasm_bindgen_futures::spawn_local(async move {
                let args = serde_json::json!({ "thread_id": tid });
                match crate::ipc::invoke::<Vec<serde_json::Value>>("side_thread_messages", &args)
                    .await
                {
                    Ok(items) => {
                        messages.set(items);
                    }
                    Err(e) => {
                        error_msg.set(Some(e));
                    }
                }
            });
        } else {
            messages.set(Vec::new());
        }
    });

    view! {
        <div class="spp-page spp-page--side-threads">
            <h2 class="spp-page__title">"Side Threads"</h2>

            <p class="spp-page__intro">
                "Internal side discussions attached to conversation #" {conversation_id.to_string()} "."
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
                <div class="spp-side-threads__layout">
                    <aside class="spp-side-threads__list">
                        <h3>"Threads"</h3>
                        <Show
                            when=move || !threads.with(|t| t.is_empty())
                            fallback=|| {
                                view! {
                                    <EmptyState message="No side threads for this conversation." />
                                }
                            }
                        >
                            <ul class="spp-side-threads__thread-list">
                                {move || threads.with(|items| {
                                    items.iter().map(|t| {
                                        let id = t.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                        let created = t.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let is_selected = move || selected_thread_id.get() == Some(id);
                                        view! {
                                            <li
                                                class="spp-side-threads__thread"
                                                class:is-selected=is_selected
                                                on:click=move |_| {
                                                    selected_thread_id.set(Some(id));
                                                }
                                            >
                                                <span class="spp-side-threads__thread-id">{"Thread #"}{id.to_string()}</span>
                                                <span class="spp-side-threads__thread-time">{created}</span>
                                            </li>
                                        }
                                    }).collect::<Vec<_>>()
                                })}
                            </ul>
                        </Show>
                    </aside>

                    <main class="spp-side-threads__messages">
                        <Show
                            when=move || selected_thread_id.get().is_some()
                            fallback=|| {
                                view! {
                                    <EmptyState message="Select a thread to view its messages." />
                                }
                            }
                        >
                            <Show
                                when=move || !messages.with(|m| m.is_empty())
                                fallback=|| {
                                    view! {
                                        <EmptyState message="No messages in this thread." />
                                    }
                                }
                            >
                                <ul class="spp-side-threads__message-list">
                                    {move || messages.with(|items| {
                                        items.iter().map(|m| {
                                            let body = m.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let created = m.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let author = m.get("author_user_id").and_then(|v| v.as_i64()).unwrap_or(0);
                                            view! {
                                                <li class="spp-side-threads__message">
                                                    <div class="spp-side-threads__message-header">
                                                        <span class="spp-side-threads__author">{"User #"}{author.to_string()}</span>
                                                        <span class="spp-side-threads__time">{created}</span>
                                                    </div>
                                                    <div class="spp-side-threads__message-body">{body}</div>
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
    // The Side Threads page's UI rendering is verified by the wasm test runner in CI.
}
