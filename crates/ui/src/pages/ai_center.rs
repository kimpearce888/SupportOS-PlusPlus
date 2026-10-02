//! AI Center page — model selection + status + Copilot allowlist.
//!
//! Per spec M6: "AI Center." Per A10: show the Copilot tool allowlist.
//! Per A5: "LM Studio is optional, never bundled: auto-detect,
//! list models, select, test. The app works fully without them."
//!
//! Every control calls a real IPC command:
//! - Page load calls `ai_status` + `copilot_allowlist`.
//! - Provider dropdown calls `ai_set_provider`.
//! - Chat model input calls `ai_set_chat_model`.
//! - Embedding model input calls `ai_set_embedding_model`.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The AI Center page.
#[component]
pub fn AiCenterPage() -> impl IntoView {
    let status = create_rw_signal(serde_json::json!({}));
    let allowlist = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    let action_msg = create_rw_signal(None::<String>);

    // Load AI status + Copilot allowlist on mount.
    create_effect(move |_| {
        let status = status;
        let allowlist = allowlist;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({});
            let status_result = crate::ipc::invoke::<serde_json::Value>("ai_status", &args).await;
            let allowlist_result =
                crate::ipc::invoke::<Vec<serde_json::Value>>("copilot_allowlist", &args).await;
            match (status_result, allowlist_result) {
                (Ok(s), Ok(a)) => {
                    status.set(s);
                    allowlist.set(a);
                    loading.set(false);
                }
                (Err(e), _) | (_, Err(e)) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    let set_provider = move |provider: String| {
        let provider_clone = provider.clone();
        let action_msg = action_msg;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "provider_kind": provider_clone });
            match crate::ipc::invoke::<()>("ai_set_provider", &args).await {
                Ok(()) => {
                    action_msg.set(Some(format!("Provider set to {provider}")));
                    error_msg.set(None);
                    // Refresh status by reloading the page state.
                    let args2 = serde_json::json!({});
                    if let Ok(s) =
                        crate::ipc::invoke::<serde_json::Value>("ai_status", &args2).await
                    {
                        status.set(s);
                    }
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    action_msg.set(None);
                }
            }
        });
    };

    let set_chat_model = move |model: String| {
        let model_clone = model.clone();
        let action_msg = action_msg;
        let error_msg = error_msg;
        let status = status;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "model": model_clone });
            match crate::ipc::invoke::<()>("ai_set_chat_model", &args).await {
                Ok(()) => {
                    action_msg.set(Some(format!("Chat model set to {model}")));
                    let args2 = serde_json::json!({});
                    if let Ok(s) =
                        crate::ipc::invoke::<serde_json::Value>("ai_status", &args2).await
                    {
                        status.set(s);
                    }
                }
                Err(e) => {
                    error_msg.set(Some(e));
                }
            }
        });
    };

    let set_embedding_model = move |model: String, dim: usize| {
        let model_clone = model.clone();
        let action_msg = action_msg;
        let error_msg = error_msg;
        let status = status;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "model": model_clone, "dim": dim });
            match crate::ipc::invoke::<()>("ai_set_embedding_model", &args).await {
                Ok(()) => {
                    action_msg.set(Some(format!("Embedding model set to {model}")));
                    let args2 = serde_json::json!({});
                    if let Ok(s) =
                        crate::ipc::invoke::<serde_json::Value>("ai_status", &args2).await
                    {
                        status.set(s);
                    }
                }
                Err(e) => {
                    error_msg.set(Some(e));
                }
            }
        });
    };

    let chat_model_input = create_rw_signal(String::new());
    let embed_model_input = create_rw_signal(String::new());
    let embed_dim_input = create_rw_signal("384".to_string());

    view! {
        <div class="spp-page spp-page--ai-center">
            <h2 class="spp-page__title">"AI Center"</h2>

            <p class="spp-page__intro">
                "Configure your local AI provider. The app works fully without AI (spec A5). "
                "LM Studio is optional, never bundled."
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

            <Show when=move || action_msg.get().is_some() fallback=|| ()>
                <div class="spp-state spp-state--success">
                    {move || action_msg.get().unwrap_or_default()}
                </div>
            </Show>

            <Show
                when=move || !loading.get() && error_msg.get().is_none()
                fallback=|| ()
            >
                {move || {
                    let s = status.get();
                    let provider_kind = s.get("provider_kind").and_then(|v| v.as_str()).unwrap_or("none").to_string();
                    let chat_model = s.get("chat_model").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let embedding_model = s.get("embedding_model").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let embedding_dim = s.get("embedding_dim").and_then(|v| v.as_u64()).map(|d| d as usize);
                    let base_url = s.get("base_url").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let provider_available = s.get("provider_available").and_then(|v| v.as_bool());
                    let chat_model_display = chat_model.clone();
                    let embed_model_display = embedding_model.clone();
                    let embed_dim_display = embedding_dim;
                    let base_url_display = base_url.clone();

                    view! {
                        <section class="spp-ai-center__section">
                            <h3>"Provider"</h3>
                            <div class="spp-ai-center__field">
                                <label>"Provider kind"</label>
                                <select
                                    class="spp-ai-center__select"
                                    value=provider_kind.clone()
                                    on:change=move |ev| {
                                        set_provider(event_target_value(&ev));
                                    }
                                >
                                    <option value="none">"None (no AI provider)"</option>
                                    <option value="lm_studio">"LM Studio"</option>
                                                                        <option value="generic">"Generic (OpenAI-compatible)"</option>
                                </select>
                            </div>

                            {if let Some(url) = base_url_display {
                                view! {
                                    <div class="spp-ai-center__field">
                                        <label>"Base URL"</label>
                                        <span class="spp-ai-center__value">{url}</span>
                                    </div>
                                }.into_view()
                            } else {
                                ().into_view()
                            }}

                            {if let Some(avail) = provider_available {
                                view! {
                                    <div class="spp-ai-center__field">
                                        <label>"Available"</label>
                                        <span class="spp-badge">
                                            {if avail { "✅ Yes" } else { "❌ No" }}
                                        </span>
                                    </div>
                                }.into_view()
                            } else {
                                ().into_view()
                            }}
                        </section>

                        <section class="spp-ai-center__section">
                            <h3>"Chat model"</h3>
                            {if let Some(cm) = chat_model_display {
                                view! {
                                    <div class="spp-ai-center__field">
                                        <label>"Current"</label>
                                        <span class="spp-ai-center__value">{cm}</span>
                                    </div>
                                }.into_view()
                            } else {
                                view! {
                                    <p class="spp-ai-center__hint">"No chat model selected."</p>
                                }.into_view()
                            }}
                            <div class="spp-ai-center__field">
                                <input
                                    class="spp-ai-center__input"
                                    type="text"
                                    placeholder="e.g. llama-3.1-8b-instruct"
                                    prop:value=chat_model_input
                                    on:input=move |ev| chat_model_input.set(event_target_value(&ev))
                                />
                                <button
                                    class="spp-button"
                                    on:click=move |_| {
                                        let m = chat_model_input.get();
                                        if !m.is_empty() {
                                            set_chat_model(m);
                                            chat_model_input.set(String::new());
                                        }
                                    }
                                >
                                    "Set chat model"
                                </button>
                            </div>
                        </section>

                        <section class="spp-ai-center__section">
                            <h3>"Embedding model"</h3>
                            {if let Some(em) = embed_model_display {
                                view! {
                                    <div class="spp-ai-center__field">
                                        <label>"Current"</label>
                                        <span class="spp-ai-center__value">
                                            {em} " (dim: " {embed_dim_display.unwrap_or(0).to_string()} ")"
                                        </span>
                                    </div>
                                }.into_view()
                            } else {
                                view! {
                                    <p class="spp-ai-center__hint">"No embedding model selected."</p>
                                }.into_view()
                            }}
                            <div class="spp-ai-center__field">
                                <input
                                    class="spp-ai-center__input"
                                    type="text"
                                    placeholder="e.g. nomic-embed-text"
                                    prop:value=embed_model_input
                                    on:input=move |ev| embed_model_input.set(event_target_value(&ev))
                                />
                                <input
                                    class="spp-ai-center__input spp-ai-center__input--dim"
                                    type="number"
                                    placeholder="dim"
                                    prop:value=embed_dim_input
                                    on:input=move |ev| embed_dim_input.set(event_target_value(&ev))
                                />
                                <button
                                    class="spp-button"
                                    on:click=move |_| {
                                        let m = embed_model_input.get();
                                        let d = embed_dim_input.get().parse::<usize>().unwrap_or(384);
                                        if !m.is_empty() {
                                            set_embedding_model(m, d);
                                            embed_model_input.set(String::new());
                                        }
                                    }
                                >
                                    "Set embedding model"
                                </button>
                            </div>
                        </section>
                    }.into_view()
                }}

                <section class="spp-ai-center__section">
                    <h3>"Copilot tool allowlist (22 read-only tools)"</h3>
                    <p class="spp-ai-center__hint">
                        "Per spec A10: the Copilot's read-only tool allowlist is shown here for transparency."
                    </p>
                    <Show
                        when=move || !allowlist.with(|a| a.is_empty())
                        fallback=|| {
                            view! {
                                <EmptyState message="No Copilot tools found." />
                            }
                        }
                    >
                        <ul class="spp-ai-center__allowlist">
                            {move || allowlist.with(|tools| {
                                tools.iter().map(|t| {
                                    let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let desc = t.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    view! {
                                        <li class="spp-ai-center__tool">
                                            <code class="spp-ai-center__tool-id">{id}</code>
                                            <span class="spp-ai-center__tool-desc">{desc}</span>
                                        </li>
                                    }
                                }).collect::<Vec<_>>()
                            })}
                        </ul>
                    </Show>
                </section>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The AI Center page's UI rendering is verified by the wasm test runner
    // in CI. This module exists to ensure the file compiles as a test target.
}
