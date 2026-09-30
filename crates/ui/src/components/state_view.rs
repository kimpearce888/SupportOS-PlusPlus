//! State views — loading, empty, error.
//!
//! Per spec KNOWN PITFALLS: "every view has loading, empty, and error states".
//! This module is the single source of truth for those three states: every
//! view that fetches data should render `<StateView view=.../>` and switch
//! on the [`ViewState`] enum.

use std::sync::Arc;

use leptos::*;

/// Which state a view is in. Wrong combinations are impossible by construction
/// — you can't have an "error with no message" or a "loading with results".
#[derive(Clone)]
pub enum ViewState {
    /// Data is being fetched. No payload yet.
    Loading,
    /// The fetch returned no data. `message` is human-readable context.
    Empty { message: String },
    /// The fetch failed. `message` is what went wrong; `retry` is optional.
    Error {
        message: String,
        retry: Option<RetryAction>,
    },
    /// The fetch succeeded; the caller renders its own children.
    Loaded,
}

/// A clonable retry callback. Wraps `Fn() + Send + Sync` in an `Arc` so the
/// `ViewState` enum can derive `Clone`.
pub type RetryAction = Arc<dyn Fn() + Send + Sync>;

impl std::fmt::Debug for ViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Loading => write!(f, "ViewState::Loading"),
            Self::Empty { message } => f
                .debug_struct("ViewState::Empty")
                .field("message", message)
                .finish(),
            Self::Error { message, retry } => f
                .debug_struct("ViewState::Error")
                .field("message", message)
                .field("retry", &retry.is_some())
                .finish(),
            Self::Loaded => write!(f, "ViewState::Loaded"),
        }
    }
}

impl ViewState {
    /// Convenience constructor for the loading state.
    #[must_use]
    pub fn loading() -> Self {
        Self::Loading
    }

    /// Convenience constructor for the empty state.
    #[must_use]
    pub fn empty(message: impl Into<String>) -> Self {
        Self::Empty {
            message: message.into(),
        }
    }

    /// Convenience constructor for the error state with no retry.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            retry: None,
        }
    }

    /// Convenience constructor for the error state with a retry callback.
    #[must_use]
    pub fn error_with_retry(
        message: impl Into<String>,
        retry: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self::Error {
            message: message.into(),
            retry: Some(Arc::new(retry)),
        }
    }
}

/// The `<StateView>` component — renders the right placeholder for the
/// current [`ViewState`]. Caller passes children for the `Loaded` case.
#[component]
pub fn StateView(
    /// The current view state.
    #[prop(into)]
    state: MaybeSignal<ViewState>,
    /// What to render when state is `Loaded`.
    children: ChildrenFn,
) -> impl IntoView {
    // `ChildrenFn` is `Fn() -> View`, callable more than once, which is what
    // we need here (the outer closure may be re-run by Leptos when `state`
    // changes).
    move || match state.get() {
        ViewState::Loading => view! { <LoadingState /> }.into_view(),
        ViewState::Empty { message } => view! {
            <EmptyState message=message.clone() />
        }
        .into_view(),
        ViewState::Error { message, retry } => view! {
            <ErrorState message=message.clone() retry=retry.clone() />
        }
        .into_view(),
        ViewState::Loaded => children().into_view(),
    }
}

/// The loading state — a spinner plus "Loading…".
#[component]
pub fn LoadingState() -> impl IntoView {
    view! {
        <div class="spp-state">
            <span class="spp-spinner" aria-label="Loading"></span>
            <p class="spp-state__body">"Loading…"</p>
        </div>
    }
}

/// The empty state — informational icon + message.
#[component]
pub fn EmptyState(#[prop(into)] message: String) -> impl IntoView {
    view! {
        <div class="spp-state">
            <span class="spp-state__icon" aria-hidden="true">"∅"</span>
            <p class="spp-state__body">{message}</p>
        </div>
    }
}

/// The error state — shows the error message and an optional retry button.
#[component]
pub fn ErrorState(
    #[prop(into)] message: String,
    /// An optional retry callback. When present, a "Try again" button is shown.
    retry: Option<RetryAction>,
) -> impl IntoView {
    let retry_signal = create_rw_signal(retry);
    let on_retry = move |_| {
        if let Some(cb) = retry_signal.get() {
            cb();
        }
    };
    view! {
        <div class="spp-state spp-state--error">
            <span class="spp-state__icon" aria-hidden="true">"⚠"</span>
            <h3 class="spp-state__title">"Something went wrong"</h3>
            <p class="spp-state__body">{message}</p>
            <Show when=move || retry_signal.get().is_some() fallback=|| ()>
                <button class="spp-button spp-button--ghost" on:click=on_retry>
                    "Try again"
                </button>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_state_constructors_set_correct_variant() {
        match ViewState::loading() {
            ViewState::Loading => {}
            _ => panic!("expected Loading"),
        }
        match ViewState::empty("nothing") {
            ViewState::Empty { message } => assert_eq!(message, "nothing"),
            _ => panic!("expected Empty"),
        }
        match ViewState::error("boom") {
            ViewState::Error { message, retry } => {
                assert_eq!(message, "boom");
                assert!(retry.is_none());
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn error_with_retry_stores_callback() {
        let called = std::sync::Arc::new(std::sync::Mutex::new(false));
        let called_clone = called.clone();
        let state = ViewState::error_with_retry("boom", move || {
            *called_clone.lock().unwrap() = true;
        });
        match state {
            ViewState::Error { message, retry } => {
                assert_eq!(message, "boom");
                let cb = retry.expect("retry should be set");
                cb();
                assert!(*called.lock().unwrap());
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn view_state_is_cloneable() {
        // Required for Leptos signals.
        let s1 = ViewState::loading();
        let s2 = s1.clone();
        match (s1, s2) {
            (ViewState::Loading, ViewState::Loading) => {}
            _ => panic!("clone mismatch"),
        }
    }

    #[test]
    fn debug_repr_does_not_panic() {
        let s = ViewState::error_with_retry("x", || {});
        let _ = format!("{:?}", s);
    }

    #[test]
    fn empty_accepts_string_or_string_literal() {
        let _ = ViewState::empty("literal");
        let _ = ViewState::empty(String::from("owned"));
    }
}
