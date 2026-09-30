//! First-run onboarding overlay (M1-T13).
//!
//! Per spec: "First run offers the 2-minute demo mode with no credentials".
//! The overlay is shown by the UI when `first_run_done` is false; the user
//! can either accept demo mode or dismiss the overlay (which marks
//! first-run done without enabling demo mode).
//!
//! The Rust-side state lives in `app_state.first_run_done` (migration 1)
//! with the typed helpers `settings::first_run_done()` /
//! `settings::mark_first_run_done()`. The Tauri IPC command
//! `first_run_state` reads/writes the flag from the UI.

use std::sync::Arc;

use leptos::*;

use crate::components::Button;

/// The props for [`OnboardingOverlay`]. Clonable so it can live inside a
/// Leptos signal; the callbacks are wrapped in `Arc` for cheap clones.
pub struct OnboardingProps {
    /// True when first-run onboarding hasn't been completed yet.
    pub first_run_done: bool,
    /// Called when the user accepts demo mode. The Tauri command
    /// `first_run_state(true)` will set `demo_mode = true` AND
    /// `first_run_done = true` in the DB.
    pub on_accept_demo: Arc<dyn Fn() + Send + Sync>,
    /// Called when the user dismisses the overlay without enabling demo mode.
    /// The Tauri command `first_run_state(false)` will set `first_run_done = true`.
    pub on_dismiss: Arc<dyn Fn() + Send + Sync>,
}

impl Clone for OnboardingProps {
    fn clone(&self) -> Self {
        Self {
            first_run_done: self.first_run_done,
            on_accept_demo: Arc::clone(&self.on_accept_demo),
            on_dismiss: Arc::clone(&self.on_dismiss),
        }
    }
}

impl std::fmt::Debug for OnboardingProps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnboardingProps")
            .field("first_run_done", &self.first_run_done)
            .field("on_accept_demo", &"<closure>")
            .field("on_dismiss", &"<closure>")
            .finish()
    }
}

impl OnboardingProps {
    /// Construct props with the two callbacks.
    #[must_use]
    pub fn new<F1, F2>(first_run_done: bool, on_accept_demo: F1, on_dismiss: F2) -> Self
    where
        F1: Fn() + Send + Sync + 'static,
        F2: Fn() + Send + Sync + 'static,
    {
        Self {
            first_run_done,
            on_accept_demo: Arc::new(on_accept_demo),
            on_dismiss: Arc::new(on_dismiss),
        }
    }
}

/// The first-run onboarding overlay.
///
/// Renders nothing (a no-op) when `first_run_done` is true. When false,
/// shows a modal with two actions:
///   - "Try the 2-minute demo mode" (no credentials required)
///   - "I'll set up later"
///
/// Both actions call back to the parent which then invokes the Tauri IPC
/// command `first_run_state(demo_mode: bool)` to persist the choice.
#[component]
pub fn OnboardingOverlay(props: OnboardingProps) -> impl IntoView {
    let visible = !props.first_run_done;
    // Pull the callbacks out into `StoredValue` so the `<Show>` body (which
    // is `Fn`) can re-invoke them on every render. `Arc<dyn Fn>` is `Copy`
    // once inside an `Arc` (cloning the Arc is cheap), but the `<Show>` body
    // closure still needs to be `Fn` not `FnOnce` — so we wrap the props in
    // a `StoredValue` and access them via `with_value`.
    let on_accept = StoredValue::new(Arc::clone(&props.on_accept_demo));
    let on_dismiss = StoredValue::new(Arc::clone(&props.on_dismiss));

    view! {
        <Show when=move || visible fallback=|| ()>
            <div class="spp-onboarding-overlay" role="dialog" aria_labelledby="spp-onboarding-title">
                <div class="spp-onboarding-card">
                    <h2 id="spp-onboarding-title" class="spp-onboarding-card__title">
                        "Welcome to SupportOS++"
                    </h2>
                    <p class="spp-onboarding-card__body">
                        "Try the 2-minute demo mode — no Help Scout credentials needed. "
                        "You can connect a real account later from Settings."
                    </p>
                    <div class="spp-onboarding-card__actions">
                        <Button
                            on_click=move || on_accept.with_value(|f| f())
                            style=crate::components::button::ButtonStyle::Primary
                        >
                            "Try the 2-minute demo mode"
                        </Button>
                        <Button
                            on_click=move || on_dismiss.with_value(|f| f())
                            style=crate::components::button::ButtonStyle::Ghost
                        >
                            "I'll set up later"
                        </Button>
                    </div>
                    <p class="spp-onboarding-card__legal">
                        "Help Scout is a trademark of Help Scout, Inc. "
                        "SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout."
                    </p>
                </div>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_construction_works() {
        let called_demo = std::sync::Arc::new(std::sync::Mutex::new(false));
        let called_dismiss = std::sync::Arc::new(std::sync::Mutex::new(false));
        let cd1 = called_demo.clone();
        let cd2 = called_dismiss.clone();
        let p = OnboardingProps::new(
            false,
            move || *cd1.lock().unwrap() = true,
            move || *cd2.lock().unwrap() = true,
        );
        (p.on_accept_demo)();
        (p.on_dismiss)();
        assert!(*called_demo.lock().unwrap());
        assert!(*called_dismiss.lock().unwrap());
    }

    #[test]
    fn props_first_run_done_field_round_trips() {
        let p1 = OnboardingProps::new(false, || {}, || {});
        assert!(!p1.first_run_done);
        let p2 = OnboardingProps::new(true, || {}, || {});
        assert!(p2.first_run_done);
    }

    #[test]
    fn props_is_cloneable() {
        // Required so the component can live inside Leptos signals.
        let p1 = OnboardingProps::new(false, || {}, || {});
        let p2 = p1.clone();
        assert_eq!(p1.first_run_done, p2.first_run_done);
    }

    #[test]
    fn props_debug_repr_does_not_panic() {
        let p = OnboardingProps::new(false, || {}, || {});
        let _ = format!("{:?}", p);
    }

    #[test]
    fn callbacks_share_state_via_arc() {
        // Both props should invoke the SAME callback state — verified by
        // cloning the props and seeing the counter increment on either side.
        let counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let c1 = counter.clone();
        let c2 = counter.clone();
        let p = OnboardingProps::new(
            false,
            move || *c1.lock().unwrap() += 1,
            move || *c2.lock().unwrap() += 1,
        );
        let p_clone = p.clone();
        (p.on_accept_demo)();
        (p_clone.on_dismiss)();
        assert_eq!(*counter.lock().unwrap(), 2);
    }
}
