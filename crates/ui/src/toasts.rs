//! Global toast store + overlay (reference `state/uiStore.ts` toasts +
//! `components/common/overlays.tsx` `Toasts`).
//!
//! The reference keeps the toast stack in the zustand `useUiStore`, so any
//! component can `pushToast` without prop threading. The port stores the
//! stack in a module-global `RwSignal` (created by [`init`] inside the
//! reactive runtime at mount) with the same API shape:
//!
//! - `push(kind, message, detail?)` — append + auto-dismiss timer
//!   (error 10s, every other kind 5s, reference `pushToast`);
//! - the stack is bounded to the newest [`MAX_TOASTS`] toasts (v1.6.0 audit
//!   fix: the unbounded stack let a burst of errors bury the UI);
//! - `dismiss(id)` — manual close (the × button).
//!
//! Rendering: [`Toasts`] is mounted once in the app shell, right after the
//! routes — the same position as the reference `App.tsx` (`<Toasts />`
//! before `<ServerEventsBridge />`). Toasts carry `role="status"` inside an
//! `aria-live="polite"` container; the optional `detail` renders behind a
//! `<details>` "Technical details" disclosure, like the reference.

use leptos::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// The v1.6.0 audit-fix bound: only the newest 5 toasts stay stacked.
pub const MAX_TOASTS: usize = 5;

/// Toast severity (reference `Toast['kind']`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
    Warning,
    Info,
}

impl ToastKind {
    /// The wire/CSS name (reference uses it directly as the class suffix).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// One toast (reference `Toast`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub message: String,
    pub detail: Option<String>,
}

static TOASTS: OnceLock<RwSignal<Vec<Toast>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Create the global stack signal. Must run inside the reactive runtime
/// (called from `app_view` before the shell mounts, like `UiState::create`).
pub fn init() {
    let _ = TOASTS.set(create_rw_signal(Vec::<Toast>::new()));
}

/// The global stack signal, if [`init`] has run.
#[must_use]
pub fn stack() -> Option<RwSignal<Vec<Toast>>> {
    TOASTS.get().copied()
}

/// Pure push core (unit-tested natively): append, then bound to the newest
/// [`MAX_TOASTS`] (v1.6.0 audit fix — older toasts are dismissed).
fn apply_push(stack: &mut Vec<Toast>, toast: Toast) {
    stack.push(toast);
    if stack.len() > MAX_TOASTS {
        let drop_n = stack.len() - MAX_TOASTS;
        stack.drain(..drop_n);
    }
}

/// Pure dismiss core (unit-tested natively): remove by id; unknown ids are
/// a no-op (an auto-dismiss timer can race a manual close — same as the
/// reference's `filter`).
fn apply_dismiss(stack: &mut Vec<Toast>, id: u64) {
    stack.retain(|t| t.id != id);
}

/// Auto-dismiss delay (reference: `t.kind === 'error' ? 10000 : 5000`).
#[must_use]
pub fn auto_dismiss_ms(kind: ToastKind) -> u64 {
    if kind == ToastKind::Error {
        10_000
    } else {
        5_000
    }
}

/// Push a toast onto the global stack and arm its auto-dismiss timer.
///
/// No-op when called before [`init`] (there is no stack to render onto —
/// the reference cannot push before the store exists either).
pub fn push(kind: ToastKind, message: impl Into<String>, detail: Option<String>) {
    let Some(stack) = stack() else { return };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let toast = Toast {
        id,
        kind,
        message: message.into(),
        detail,
    };
    stack.update(|v| apply_push(v, toast));
    let delay = auto_dismiss_ms(kind);
    set_timeout(move || dismiss(id), std::time::Duration::from_millis(delay));
}

/// `push(ToastKind::Success, message, None)`
pub fn success(message: impl Into<String>) {
    push(ToastKind::Success, message, None);
}

/// `push(ToastKind::Error, message, None)`
pub fn error(message: impl Into<String>) {
    push(ToastKind::Error, message, None);
}

/// `push(ToastKind::Warning, message, None)`
pub fn warning(message: impl Into<String>) {
    push(ToastKind::Warning, message, None);
}

/// `push(ToastKind::Info, message, None)`
pub fn info(message: impl Into<String>) {
    push(ToastKind::Info, message, None);
}

/// Dismiss one toast by id (the × button; also the auto-dismiss timer).
pub fn dismiss(id: u64) {
    if let Some(stack) = stack() {
        stack.update(|v| apply_dismiss(v, id));
    }
}

/// The toast overlay (reference `overlays.tsx` `Toasts`): fixed stack in the
/// bottom-right corner. Mounted once by the app shell.
#[component]
pub fn Toasts() -> impl IntoView {
    let stack = TOASTS
        .get()
        .copied()
        .expect("toasts::init() must run in app_view before <Toasts/> mounts");
    view! {
        <div class="spp-toast-container" aria-live="polite">
            <For
                each=move || stack.get()
                key=|t| t.id
                children=move |t: Toast| {
                    let id = t.id;
                    let class = format!("spp-toast spp-toast--{}", t.kind.as_str());
                    // The optional detail renders behind a disclosure, like
                    // the reference (message first, details on demand).
                    let detail_view = t.detail.as_deref().map(|d| {
                        view! {
                            <details class="spp-toast__details">
                                <summary class="spp-muted spp-text-xs">"Technical details"</summary>
                                <div class="spp-toast__detail-body">{d.to_string()}</div>
                            </details>
                        }
                        .into_view()
                    });
                    view! {
                        <div class=class role="status">
                            <div class="spp-toast__row">
                                <div class="spp-toast__body">
                                    {t.message.clone()}
                                    {detail_view}
                                </div>
                                <button
                                    class="spp-button spp-button--ghost spp-button--tiny"
                                    aria-label="Dismiss"
                                    on:click=move |_| dismiss(id)
                                >
                                    "\u{d7}"
                                </button>
                            </div>
                        </div>
                    }
                }
            />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(id: u64, kind: ToastKind) -> Toast {
        Toast {
            id,
            kind,
            message: format!("m{id}"),
            detail: if id % 2 == 0 {
                Some("d".to_string())
            } else {
                None
            },
        }
    }

    #[test]
    fn kind_names_match_reference() {
        assert_eq!(ToastKind::Success.as_str(), "success");
        assert_eq!(ToastKind::Error.as_str(), "error");
        assert_eq!(ToastKind::Warning.as_str(), "warning");
        assert_eq!(ToastKind::Info.as_str(), "info");
    }

    #[test]
    fn push_appends_in_order() {
        let mut stack = Vec::new();
        apply_push(&mut stack, toast(1, ToastKind::Success));
        apply_push(&mut stack, toast(2, ToastKind::Error));
        assert_eq!(stack.len(), 2);
        assert_eq!(stack[0].id, 1);
        assert_eq!(stack[1].id, 2);
    }

    #[test]
    fn push_bounds_stack_to_newest_five() {
        // v1.6.0 audit fix: the unbounded stack buried the UI under a burst
        // of errors. Keep the newest 5; older ones are dismissed.
        let mut stack = Vec::new();
        for id in 1..=8 {
            apply_push(&mut stack, toast(id, ToastKind::Error));
        }
        assert_eq!(stack.len(), MAX_TOASTS);
        let ids: Vec<u64> = stack.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![4, 5, 6, 7, 8]);
    }

    #[test]
    fn dismiss_removes_only_that_id() {
        let mut stack = Vec::new();
        for id in 1..=3 {
            apply_push(&mut stack, toast(id, ToastKind::Info));
        }
        apply_dismiss(&mut stack, 2);
        let ids: Vec<u64> = stack.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn dismiss_of_unknown_id_is_a_no_op() {
        // An auto-dismiss timer can race a manual close (both fire for the
        // same id); the loser must not disturb the stack.
        let mut stack = Vec::new();
        apply_push(&mut stack, toast(1, ToastKind::Success));
        apply_dismiss(&mut stack, 99);
        apply_dismiss(&mut stack, 1);
        apply_dismiss(&mut stack, 1);
        assert!(stack.is_empty());
    }

    #[test]
    fn auto_dismiss_matches_reference_delays() {
        assert_eq!(auto_dismiss_ms(ToastKind::Error), 10_000);
        assert_eq!(auto_dismiss_ms(ToastKind::Success), 5_000);
        assert_eq!(auto_dismiss_ms(ToastKind::Warning), 5_000);
        assert_eq!(auto_dismiss_ms(ToastKind::Info), 5_000);
    }

    #[test]
    fn detail_is_optional_like_reference() {
        let mut stack = Vec::new();
        apply_push(&mut stack, toast(2, ToastKind::Error));
        assert_eq!(stack[0].detail.as_deref(), Some("d"));
        apply_push(&mut stack, toast(3, ToastKind::Error));
        assert_eq!(stack[1].detail, None);
    }
}
