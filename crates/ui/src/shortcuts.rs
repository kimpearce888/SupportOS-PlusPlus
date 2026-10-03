//! Global keyboard shortcuts — the port of the reference `App.tsx` keydown
//! handler (spec #94):
//!
//! - `Cmd/Ctrl+K` opens the command palette (works even while typing)
//! - `/` navigates to `/search` (quick search)
//! - `g` then `d` / `i` / `s` navigates to `/` / `/inbox` / `/sync-health`
//!
//! Plain-key shortcuts are suppressed while typing in an `INPUT`,
//! `TEXTAREA`, or `contentEditable` element. The `g` sequence re-checks the
//! typing context on the follow-up key: focus may have moved into a reply
//! box between the `g` press and the `d` (typing `d` as the first character
//! of a reply must not navigate away).

use wasm_bindgen::JsCast;

/// What a resolved keydown should trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutAction {
    /// No action.
    None,
    /// `Cmd/Ctrl+K` — open the command palette (works while typing).
    OpenPalette,
    /// `/` — quick search; navigate to `/search`.
    QuickSearch,
    /// `g`+`d` / `g`+`i` / `g`+`s` — navigate to the given path.
    Navigate(&'static str),
}

/// The result of resolving one keydown against the shortcut state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutOutcome {
    /// The action this keydown triggers.
    pub action: ShortcutAction,
    /// Whether a pressed `g` is pending its follow-up key. The reference
    /// implements this as a one-shot `keydown` listener: any next key
    /// consumes the pending state, but only `d`/`i`/`s` navigate.
    pub pending_g: bool,
}

/// Resolve one keydown.
///
/// - `pending_g` — a `g` was pressed previously and not yet consumed.
/// - `typing` — the event target is an input/textarea/contentEditable.
/// - `mod_k` — the key is `k` with Cmd/Ctrl held.
/// - `key` — the `KeyboardEvent.key` value.
#[must_use]
pub fn resolve(pending_g: bool, typing: bool, mod_k: bool, key: &str) -> ShortcutOutcome {
    // Cmd/Ctrl+K is checked BEFORE the typing guard (the reference opens the
    // palette even when focus is in an input), and it consumes a pending `g`.
    if mod_k {
        return ShortcutOutcome {
            action: ShortcutAction::OpenPalette,
            pending_g: false,
        };
    }
    // While typing, plain shortcuts are dead — and any pending `g` is
    // consumed without navigating (the reference's one-shot listener fires,
    // sees the typing context, and removes itself).
    if typing {
        return ShortcutOutcome {
            action: ShortcutAction::None,
            pending_g: false,
        };
    }
    if key == "/" {
        return ShortcutOutcome {
            action: ShortcutAction::QuickSearch,
            pending_g: false,
        };
    }
    if key == "g" {
        // Re-pressing `g` keeps the sequence armed (pressing `g` twice then
        // `d` still navigates, matching the reference).
        return ShortcutOutcome {
            action: ShortcutAction::None,
            pending_g: true,
        };
    }
    if pending_g {
        let action = match key {
            "d" => ShortcutAction::Navigate("/"),
            "i" => ShortcutAction::Navigate("/inbox"),
            "s" => ShortcutAction::Navigate("/sync-health"),
            _ => ShortcutAction::None,
        };
        return ShortcutOutcome {
            action,
            pending_g: false,
        };
    }
    ShortcutOutcome {
        action: ShortcutAction::None,
        pending_g: false,
    }
}

/// The typing-context guard (reference: `target.tagName === 'INPUT' ||
/// target.tagName === 'TEXTAREA' || target.isContentEditable`).
///
/// Returns `false` when there is no element target (e.g. focus on `document`)
/// — same net effect as the reference reading undefined-ish properties.
#[must_use]
pub fn is_typing(target: Option<&web_sys::EventTarget>) -> bool {
    target
        .and_then(|t| t.dyn_ref::<web_sys::HtmlElement>())
        .is_some_and(|el| {
            let tag = el.tag_name();
            tag == "INPUT" || tag == "TEXTAREA" || el.is_content_editable()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_k_opens_palette_even_while_typing() {
        let out = resolve(false, true, true, "k");
        assert_eq!(out.action, ShortcutAction::OpenPalette);
        assert!(!out.pending_g);
        let out = resolve(false, false, true, "K");
        assert_eq!(out.action, ShortcutAction::OpenPalette);
    }

    #[test]
    fn mod_k_consumes_pending_g() {
        // Reference: the one-shot follow-up listener fires on the Cmd+K
        // keydown too (and 'k' is not d/i/s), so the sequence is consumed.
        let out = resolve(true, false, true, "k");
        assert_eq!(out.action, ShortcutAction::OpenPalette);
        assert!(!out.pending_g);
    }

    #[test]
    fn slash_navigates_to_search() {
        let out = resolve(false, false, false, "/");
        assert_eq!(out.action, ShortcutAction::QuickSearch);
        assert!(!out.pending_g);
    }

    #[test]
    fn slash_is_dead_while_typing() {
        let out = resolve(false, true, false, "/");
        assert_eq!(out.action, ShortcutAction::None);
    }

    #[test]
    fn typing_does_not_arm_g() {
        let out = resolve(false, true, false, "g");
        assert_eq!(out.action, ShortcutAction::None);
        assert!(!out.pending_g);
    }

    #[test]
    fn g_sequence_navigates() {
        let armed = resolve(false, false, false, "g");
        assert_eq!(armed.action, ShortcutAction::None);
        assert!(armed.pending_g);

        let d = resolve(true, false, false, "d");
        assert_eq!(d.action, ShortcutAction::Navigate("/"));
        assert!(!d.pending_g);

        let armed = resolve(false, false, false, "g");
        let i = resolve(armed.pending_g, false, false, "i");
        assert_eq!(i.action, ShortcutAction::Navigate("/inbox"));

        let armed = resolve(false, false, false, "g");
        let s = resolve(armed.pending_g, false, false, "s");
        assert_eq!(s.action, ShortcutAction::Navigate("/sync-health"));
    }

    #[test]
    fn any_key_consumes_pending_g() {
        for key in ["x", "j", "Enter", "k"] {
            let out = resolve(true, false, false, key);
            assert_eq!(out.action, ShortcutAction::None, "key {key}");
            assert!(!out.pending_g, "key {key}");
        }
    }

    #[test]
    fn g_then_typing_consumes_without_navigating() {
        // Focus moved into a reply box between 'g' and 'd': the 'd' must
        // land in the textarea, not navigate away.
        let out = resolve(true, true, false, "d");
        assert_eq!(out.action, ShortcutAction::None);
        assert!(!out.pending_g);
    }

    #[test]
    fn double_g_keeps_sequence_armed() {
        let one = resolve(false, false, false, "g");
        let two = resolve(one.pending_g, false, false, "g");
        assert!(two.pending_g);
        let d = resolve(two.pending_g, false, false, "d");
        assert_eq!(d.action, ShortcutAction::Navigate("/"));
    }

    #[test]
    fn no_target_is_not_typing() {
        assert!(!is_typing(None));
    }
}
