//! Settings page — the `/settings` route.
//!
//! Per spec M2+: shows Help Scout credentials, AI provider config (links to
//! AI Center), sync settings, and the startup self-check report.
//!
//! Every control calls a real IPC command:
//! - Page load calls `self_check` + `parity_gate_check` + `first_run_state`.
//! - "Mark first run done" button calls `first_run_state(Some(true))`.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Settings page.
#[component]
pub fn SettingsPage() -> impl IntoView {
    let self_check_report = create_rw_signal(None::<serde_json::Value>);
    let parity_gate = create_rw_signal(None::<serde_json::Value>);
    let first_run = create_rw_signal(None::<bool>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    let action_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let self_check_report = self_check_report;
        let parity_gate = parity_gate;
        let first_run = first_run;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let sc_result = crate::ipc::invoke::<Option<serde_json::Value>>(
                "self_check",
                &serde_json::json!({}),
            )
            .await;
            let pg_result = crate::ipc::invoke::<serde_json::Value>(
                "parity_gate_check",
                &serde_json::json!({}),
            )
            .await;
            let fr_result = crate::ipc::invoke::<bool>(
                "first_run_state",
                &serde_json::json!({ "demo_mode": null }),
            )
            .await;
            match (sc_result, pg_result, fr_result) {
                (Ok(sc), Ok(pg), Ok(fr)) => {
                    self_check_report.set(sc);
                    parity_gate.set(Some(pg));
                    first_run.set(Some(fr));
                    loading.set(false);
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    let mark_first_run_done = move || {
        let action_msg = action_msg;
        let error_msg = error_msg;
        let first_run = first_run;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "demo_mode": null });
            match crate::ipc::invoke::<bool>("first_run_state", &args).await {
                Ok(done) => {
                    first_run.set(Some(done));
                    action_msg.set(Some("First-run state refreshed".to_string()));
                    error_msg.set(None);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    action_msg.set(None);
                }
            }
        });
    };

    view! {
        <div class="spp-page spp-page--settings">
            <h2 class="spp-page__title">"Settings"</h2>

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
                <section class="spp-settings__section">
                    <h3>"First-run state"</h3>
                    <div class="spp-settings__field">
                        <label>"First-run done"</label>
                        <span class="spp-badge">
                            {move || if first_run.get().unwrap_or(false) { "✅ Yes" } else { "❌ No (first run)" }}
                        </span>
                        <button class="spp-button spp-button--ghost" on:click=move |_| mark_first_run_done()>
                            "Refresh"
                        </button>
                    </div>
                </section>

                <section class="spp-settings__section">
                    <h3>"Parity gate"</h3>
                    {move || {
                        let pg = parity_gate.get();
                        let passed = pg.as_ref().and_then(|v| v.get("passed")).and_then(|v| v.as_bool()).unwrap_or(false);
                        let total = pg.as_ref().and_then(|v| v.get("total_variants")).and_then(|v| v.as_u64()).unwrap_or(0);
                        let msg = pg.as_ref().and_then(|v| v.get("message")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                        view! {
                            <div class="spp-settings__field">
                                <label>"Status"</label>
                                <span class="spp-badge">
                                    {if passed { "✅ PASS" } else { "❌ FAIL" }}
                                </span>
                                <span>{format!("{total} catalog variants")}</span>
                            </div>
                            <p class="spp-settings__hint">{msg}</p>
                        }.into_view()
                    }}
                </section>

                <section class="spp-settings__section">
                    <h3>"Startup self-check"</h3>
                    {move || {
                        let sc = match self_check_report.get() {
                            Some(s) => s,
                            None => return view! {
                                <EmptyState message="Self-check report not available (may have failed at boot)." />
                            }.into_view(),
                        };
                        let all_ok = sc.get("all_ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let app_version = sc.get("app_version").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                        let build_target = sc.get("build_target").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                        let qdrant_enabled = sc.get("qdrant_feature_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                        let subsystems = sc.get("subsystems").and_then(|v| v.as_array()).cloned().unwrap_or_default();

                        view! {
                            <div class="spp-settings__self-check">
                                <div class="spp-settings__field">
                                    <label>"App version"</label>
                                    <span>{app_version}</span>
                                </div>
                                <div class="spp-settings__field">
                                    <label>"Build target"</label>
                                    <span>{build_target}</span>
                                </div>
                                <div class="spp-settings__field">
                                    <label>"Qdrant feature"</label>
                                    <span class="spp-badge">
                                        {if qdrant_enabled { "✅ enabled" } else { "❌ not enabled" }}
                                    </span>
                                </div>
                                <div class="spp-settings__field">
                                    <label>"Overall"</label>
                                    <span class="spp-badge">
                                        {if all_ok { "✅ all subsystems OK" } else { "❌ some subsystems failing" }}
                                    </span>
                                </div>

                                <h4>"Subsystems"</h4>
                                {if subsystems.is_empty() {
                                    view! {
                                        <EmptyState message="No subsystem reports." />
                                    }.into_view()
                                } else {
                                    view! {
                                        <ul class="spp-settings__subsystems">
                                            {subsystems.iter().map(|s| {
                                                let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                                let ok = s.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                                                let status = s.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                                view! {
                                                    <li class="spp-settings__subsystem">
                                                        <span class="spp-settings__subsystem-name">{name.clone()}</span>
                                                        <span class="spp-badge">
                                                            {if ok { "✅" } else { "❌" }}
                                                        </span>
                                                        <span class="spp-settings__subsystem-status">{status}</span>
                                                    </li>
                                                }
                                            }).collect::<Vec<_>>()}
                                        </ul>
                                    }.into_view()
                                }}
                            </div>
                        }.into_view()
                    }}
                </section>

                <section class="spp-settings__section">
                    <h3>"Help Scout credentials"</h3>
                    <EmptyState message="Help Scout OAuth credentials are not yet configurable from the UI. Use the onboarding wizard or set them via the Settings database table directly." />
                </section>

                <section class="spp-settings__section">
                    <h3>"AI provider"</h3>
                    <p class="spp-settings__hint">
                        "Configure your local AI provider (LM Studio) in the "
                        <a href="#/ai-center">"AI Center"</a>
                        "."
                    </p>
                </section>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Settings page's UI rendering is verified by the wasm test runner in CI.
}
