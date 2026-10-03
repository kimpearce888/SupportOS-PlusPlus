//! AI Center page — AI status + Copilot tool allowlist.
//!
//! Reference contract (AiCenter.tsx): reads `GET /api/ai/status`,
//! `GET /api/ai/analytics`, `GET /api/ai/jobs`, `GET /api/ai/evaluation`,
//! and `GET /api/copilot/tools`. The reference configures LM Studio via
//! Settings (LMSTUDIO_* env vars / settings keys) — there is no
//! set-provider/set-model surface here, so this page is read-only status.
//! Per A5: "LM Studio is optional, never bundled: the app works fully
//! without it."

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The AI Center page.
#[component]
pub fn AiCenterPage() -> impl IntoView {
    let status = create_rw_signal(serde_json::json!({}));
    let jobs = create_rw_signal(Vec::<serde_json::Value>::new());
    let allowlist = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    // Load AI status, recent jobs, and the Copilot tool allowlist on mount.
    create_effect(move |_| {
        let status = status;
        let jobs = jobs;
        let allowlist = allowlist;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let status_result = crate::api::get_json::<serde_json::Value>("/api/ai/status").await;
            let jobs_result = crate::api::get_json::<serde_json::Value>("/api/ai/jobs").await;
            let tools_result =
                crate::api::get_json::<serde_json::Value>("/api/copilot/tools").await;
            match (status_result, jobs_result, tools_result) {
                (Ok(s), Ok(j), Ok(t)) => {
                    status.set(s);
                    jobs.set(
                        j.get("jobs")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default(),
                    );
                    allowlist.set(
                        t.get("tools")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default(),
                    );
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
        <div class="spp-page spp-page--ai-center">
            <h2 class="spp-page__title">"AI Center"</h2>

            <p class="spp-page__intro">
                "Local AI status and the Copilot tool allowlist. The app works fully without AI. "
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

                    view! {
                        <section class="spp-ai-center__section">
                            <h3>"Provider"</h3>
                            <div class="spp-ai-center__field">
                                <label>"Provider kind"</label>
                                <span class="spp-ai-center__value">{provider_kind}</span>
                            </div>

                            {if let Some(url) = base_url {
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
                            {if let Some(cm) = chat_model {
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
                        </section>

                        <section class="spp-ai-center__section">
                            <h3>"Embedding model"</h3>
                            {if let Some(em) = embedding_model {
                                view! {
                                    <div class="spp-ai-center__field">
                                        <label>"Current"</label>
                                        <span class="spp-ai-center__value">
                                            {em} " (dim: " {embedding_dim.unwrap_or(0).to_string()} ")"
                                        </span>
                                    </div>
                                }.into_view()
                            } else {
                                view! {
                                    <p class="spp-ai-center__hint">"No embedding model selected."</p>
                                }.into_view()
                            }}
                        </section>
                    }.into_view()
                }}

                <section class="spp-ai-center__section">
                    <h3>"Recent AI jobs"</h3>
                    <Show
                        when=move || !jobs.get().is_empty()
                        fallback=|| {
                            view! {
                                <EmptyState message="No AI jobs have run yet." />
                            }
                        }
                    >
                        <ul class="spp-ai-center__jobs">
                            {move || jobs.get().iter().map(|j| {
                                let kind = j.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let state = j.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let model = j.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let created = j.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                view! {
                                    <li class="spp-ai-center__job">
                                        <span class="spp-ai-center__tool-id">{kind}</span>
                                        <span class="spp-badge">{state}</span>
                                        <span class="spp-ai-center__tool-desc">{model}</span>
                                        <span class="spp-ai-center__hint">{created}</span>
                                    </li>
                                }
                            }).collect::<Vec<_>>()}
                        </ul>
                    </Show>
                </section>

                <section class="spp-ai-center__section">
                    <h3>"Copilot tool allowlist"</h3>
                    <p class="spp-ai-center__hint">
                        "The Copilot's read-only tool allowlist, shown for transparency."
                    </p>
                    <Show
                        when=move || !allowlist.get().is_empty()
                        fallback=|| {
                            view! {
                                <EmptyState message="No Copilot tools found." />
                            }
                        }
                    >
                        <ul class="spp-ai-center__allowlist">
                            {move || allowlist.get().iter().map(|t| {
                                let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let desc = t.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                view! {
                                    <li class="spp-ai-center__tool">
                                        <code class="spp-ai-center__tool-id">{id}</code>
                                        <span class="spp-ai-center__tool-desc">{desc}</span>
                                    </li>
                                }
                            }).collect::<Vec<_>>()}
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
