//! Onboarding wizard page — first-run setup.
//!
//! Per spec M2: "first-run onboarding." Per A10: demo mode offer.
//! Calls `first_run_state` IPC to read/write the first-run flag.
//! On completion it flips the app-wide onboarding state (so the shell guard
//! sees `completed=true` immediately, like the reference's optimistic
//! `setQueryData`) and returns the user to the dashboard.

use leptos::*;
use leptos_router::{use_navigate, NavigateOptions};

/// The Onboarding page.
#[component]
pub fn OnboardingPage() -> impl IntoView {
    let first_run_done = create_rw_signal(None::<bool>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    let action_msg = create_rw_signal(None::<String>);

    // App-wide state (guard + shell visibility); absent when the page is
    // rendered outside the app shell (e.g. isolated tests).
    let ui = use_context::<crate::state::UiState>();
    let navigate = use_navigate();

    create_effect(move |_| {
        let first_run_done = first_run_done;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "demo_mode": null });
            match crate::ipc::invoke::<bool>("first_run_state", &args).await {
                Ok(done) => {
                    first_run_done.set(Some(done));
                    if let Some(ui) = ui {
                        ui.onboarding_completed.set(Some(done));
                    }
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    let mark_done = move |demo: bool| {
        let action_msg = action_msg;
        let error_msg = error_msg;
        let first_run_done = first_run_done;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({ "demo_mode": demo });
            match crate::ipc::invoke::<bool>("first_run_state", &args).await {
                Ok(done) => {
                    first_run_done.set(Some(done));
                    // Optimistically flip the app-wide state BEFORE anything
                    // else runs, so the guard does not bounce the user back
                    // with stale `completed=false` data (reference v1.6.0
                    // audit fix for the finish race).
                    if let Some(ui) = ui {
                        ui.onboarding_completed.set(Some(true));
                    }
                    if demo {
                        action_msg.set(Some(
                            "Demo mode enabled. First-run marked done.".to_string(),
                        ));
                    } else {
                        action_msg.set(Some(
                            "First-run marked done. You can set up later.".to_string(),
                        ));
                    }
                    error_msg.set(None);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    action_msg.set(None);
                }
            }
        });
    };

    // Return to the dashboard when the wizard completes (reference: the
    // finish mutation navigates to '/'). Fires only on the *transition* to
    // done — a returning user who opens /onboarding directly stays put.
    create_effect(move |prev: Option<Option<bool>>| {
        let done = first_run_done.get();
        if prev.is_some() && done == Some(true) {
            navigate("/", NavigateOptions::default());
        }
        done
    });

    view! {
        <div class="spp-page spp-page--onboarding">
            <h2 class="spp-page__title">"Onboarding"</h2>

            <p class="spp-page__intro">
                "Welcome to SupportOS++. Choose how you'd like to get started."
            </p>

            <Show when=move || loading.get() fallback=|| ()>
                <p>"Loading…"</p>
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
                    let done = first_run_done.get().unwrap_or(false);
                    if done {
                        view! {
                            <div class="spp-onboarding__done">
                                <h3>"✅ Setup complete"</h3>
                                <p>"You've already completed the first-run onboarding."</p>
                                <p>"To connect Help Scout, go to Settings → Help Scout credentials."</p>
                                <p>"To configure AI, go to the AI Center."</p>
                            </div>
                        }.into_view()
                    } else {
                        view! {
                            <div class="spp-onboarding__choices">
                                <div class="spp-onboarding__choice">
                                    <h3>"Try the 2-minute demo mode"</h3>
                                    <p>"No credentials needed. The app loads sample data so you can explore the UI immediately."</p>
                                    <button class="spp-button" on:click=move |_| mark_done(true)>
                                        "Start demo mode"
                                    </button>
                                </div>

                                <div class="spp-onboarding__choice">
                                    <h3>"I'll set up later"</h3>
                                    <p>"Mark the first-run as done. You can configure Help Scout credentials and AI providers at any time from Settings."</p>
                                    <button class="spp-button spp-button--ghost" on:click=move |_| mark_done(false)>
                                        "Skip for now"
                                    </button>
                                </div>
                            </div>

                            <div class="spp-onboarding__help">
                                <h3>"What you'll need to set up later"</h3>
                                <ul>
                                    <li>"Help Scout OAuth client ID + secret (for real data sync)"</li>
                                    <li>"A local AI provider: LM Studio (127.0.0.1:1234) or Ollama (127.0.0.1:11434) — both optional"</li>
                                    <li>"No cloud accounts, no telemetry, no data egress — everything stays local."</li>
                                </ul>
                            </div>
                        }.into_view()
                    }
                }}
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Onboarding page's UI rendering is verified by the wasm test runner in CI.
}
