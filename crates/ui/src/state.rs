//! App UI state — the Leptos mirror of the reference's zustand
//! `src/client/state/uiStore.ts`.
//!
//! Persisted in `localStorage` with the reference's exact keys and values:
//! - `supportos-theme` — `"light"` | `"dark"` (reference default: light)
//! - `supportos-sidebar` — `"collapsed"` | `"open"` (reference default: open)
//!
//! Not persisted (same as the reference store): command-palette open state,
//! the onboarding status used by the first-run guard, and the nav badge
//! counts (30s-poll + SSE-refreshed).

use leptos::*;

/// `localStorage` key for the color theme (reference uiStore).
pub const THEME_STORAGE_KEY: &str = "supportos-theme";

/// `localStorage` key for the sidebar collapse state (reference uiStore).
pub const SIDEBAR_STORAGE_KEY: &str = "supportos-sidebar";

/// The two color themes. Applied as `data-theme` on `<html>`
/// (reference: `document.documentElement.setAttribute('data-theme', …)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    /// The `data-theme` attribute value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Parse a stored value. Reference default: `?? 'light'` — anything that
    /// is not `"dark"` (missing, `"light"`, or garbage) means light.
    #[must_use]
    pub fn from_stored(stored: Option<&str>) -> Self {
        match stored {
            Some("dark") => Self::Dark,
            _ => Self::Light,
        }
    }

    /// The theme the toggle button switches to.
    #[must_use]
    pub fn toggle(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    /// Set `data-theme` on the document element. No-ops outside a browser
    /// (native tests have no DOM — `web_sys::window()` panics on non-wasm
    /// targets rather than returning `None`).
    pub fn apply(self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Some(html) = doc.document_element() {
                let _ = html.set_attribute("data-theme", self.as_str());
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = self;
    }
}

fn local_storage_get(key: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item(key).ok().flatten())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
        None
    }
}

fn local_storage_set(key: &str, value: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(key, value);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (key, value);
}

/// Apply the persisted theme to `<html>` before the first render, so the
/// correct token set is active when the app mounts (no theme flash).
pub fn apply_theme_at_boot() {
    Theme::from_stored(local_storage_get(THEME_STORAGE_KEY).as_deref()).apply();
}

/// The app-wide UI state, provided as context from the root `app_view` and
/// consumed by the layout shell, the command palette, and the app-level
/// effects (onboarding guard, nav counts, shortcuts).
#[derive(Clone, Copy)]
pub struct UiState {
    /// Color theme (`supportos-theme`).
    pub theme: RwSignal<Theme>,
    /// Sidebar collapsed (`supportos-sidebar`).
    pub sidebar_collapsed: RwSignal<bool>,
    /// Command palette open (not persisted).
    pub palette_open: RwSignal<bool>,
    /// Onboarding status from `/api/onboarding`. `None` = unknown (fetch
    /// pending or failed) — the guard does not redirect and the shell shows,
    /// exactly like the reference's `undefined` query data.
    pub onboarding_completed: RwSignal<Option<bool>>,
    /// Active-conversation count for the Inbox nav badge
    /// (`/api/conversations?view=active&pageSize=1` → `total`).
    pub inbox_count: RwSignal<Option<u32>>,
    /// Unread-notification count for the Notifications nav badge
    /// (`/api/notifications/unread-count` → `unread`).
    pub unread_count: RwSignal<Option<u32>>,
}

impl UiState {
    /// Create the state, hydrating theme + sidebar from `localStorage`.
    /// Must run inside the reactive runtime (i.e. in `app_view`).
    #[must_use]
    pub fn create() -> Self {
        let theme = Theme::from_stored(local_storage_get(THEME_STORAGE_KEY).as_deref());
        let collapsed = local_storage_get(SIDEBAR_STORAGE_KEY).as_deref() == Some("collapsed");
        Self {
            theme: create_rw_signal(theme),
            sidebar_collapsed: create_rw_signal(collapsed),
            palette_open: create_rw_signal(false),
            onboarding_completed: create_rw_signal(None),
            inbox_count: create_rw_signal(None),
            unread_count: create_rw_signal(None),
        }
    }

    /// Toggle + persist + apply the color theme.
    pub fn toggle_theme(&self) {
        let next = self.theme.get().toggle();
        self.theme.set(next);
        local_storage_set(THEME_STORAGE_KEY, next.as_str());
        next.apply();
    }

    /// Toggle + persist the sidebar collapse state.
    pub fn toggle_sidebar(&self) {
        let next = !self.sidebar_collapsed.get();
        self.sidebar_collapsed.set(next);
        local_storage_set(SIDEBAR_STORAGE_KEY, if next { "collapsed" } else { "open" });
    }

    /// Refresh both nav badge counts (reference: `['nav-counts']` and
    /// `['notification-unread']` queries with a 30s refetch interval).
    /// Failures leave the previous values in place — a dead badge is better
    /// than a flickering one.
    pub fn refresh_nav_counts(&self) {
        let inbox = self.inbox_count;
        let unread = self.unread_count;
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(v) = crate::api::get_json::<serde_json::Value>(
                "/api/conversations?view=active&pageSize=1",
            )
            .await
            {
                if let Some(total) = v.get("total").and_then(serde_json::Value::as_u64) {
                    inbox.set(Some(u32::try_from(total).unwrap_or(u32::MAX)));
                }
            }
            if let Ok(v) =
                crate::api::get_json::<serde_json::Value>("/api/notifications/unread-count").await
            {
                if let Some(count) = v.get("unread").and_then(serde_json::Value::as_u64) {
                    unread.set(Some(u32::try_from(count).unwrap_or(u32::MAX)));
                }
            }
        });
    }
}

/// Nav badge text: hidden at 0, capped at `"99+"` (reference `NavItem`:
/// `count != null && count > 0 ? (count > 99 ? '99+' : count) : null`).
#[must_use]
pub fn badge_text(count: u32) -> Option<String> {
    if count == 0 {
        None
    } else if count > 99 {
        Some("99+".to_string())
    } else {
        Some(count.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_defaults_to_light_like_reference() {
        assert_eq!(Theme::from_stored(None), Theme::Light);
        assert_eq!(Theme::from_stored(Some("light")), Theme::Light);
        assert_eq!(Theme::from_stored(Some("dark")), Theme::Dark);
        // Garbage values fall back to light (the reference's CSS would fall
        // back to the :root light block for unknown data-theme values too).
        assert_eq!(Theme::from_stored(Some("blue")), Theme::Light);
    }

    #[test]
    fn theme_toggle_round_trips() {
        assert_eq!(Theme::Light.toggle(), Theme::Dark);
        assert_eq!(Theme::Dark.toggle(), Theme::Light);
        assert_eq!(Theme::Light.toggle().toggle(), Theme::Light);
    }

    #[test]
    fn theme_attribute_values_match_reference() {
        assert_eq!(Theme::Light.as_str(), "light");
        assert_eq!(Theme::Dark.as_str(), "dark");
    }

    #[test]
    fn storage_keys_match_reference_ui_store() {
        assert_eq!(THEME_STORAGE_KEY, "supportos-theme");
        assert_eq!(SIDEBAR_STORAGE_KEY, "supportos-sidebar");
    }

    #[test]
    fn badge_text_matches_reference_nav_item() {
        assert_eq!(badge_text(0), None);
        assert_eq!(badge_text(1), Some("1".to_string()));
        assert_eq!(badge_text(99), Some("99".to_string()));
        assert_eq!(badge_text(100), Some("99+".to_string()));
        assert_eq!(badge_text(9_999), Some("99+".to_string()));
    }

    #[test]
    fn ui_state_hydrates_from_empty_storage() {
        // No browser in native tests: localStorage reads are None, so the
        // defaults (light theme, expanded sidebar) must hold.
        let _runtime = create_runtime();
        let ui = UiState::create();
        assert_eq!(ui.theme.get(), Theme::Light);
        assert!(!ui.sidebar_collapsed.get());
        assert!(!ui.palette_open.get());
        assert_eq!(ui.onboarding_completed.get(), None);
        assert_eq!(ui.inbox_count.get(), None);
        assert_eq!(ui.unread_count.get(), None);
    }

    #[test]
    fn ui_state_toggles_flip_signals() {
        let _runtime = create_runtime();
        let ui = UiState::create();
        ui.toggle_sidebar();
        assert!(ui.sidebar_collapsed.get());
        ui.toggle_sidebar();
        assert!(!ui.sidebar_collapsed.get());
        ui.toggle_theme();
        assert_eq!(ui.theme.get(), Theme::Dark);
        ui.toggle_theme();
        assert_eq!(ui.theme.get(), Theme::Light);
    }
}
