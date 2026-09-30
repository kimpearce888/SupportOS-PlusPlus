//! Button component.
//!
//! A small wrapper around `<button>` with the `spp-button` class. Keeps the
//! class name and ARIA conventions in one place (A12: never write the same
//! logic twice).

use leptos::*;

/// The visual style of a [`Button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ButtonStyle {
    /// Filled accent background (the default action).
    #[default]
    Primary,
    /// Outlined, transparent background — for secondary actions.
    Ghost,
}

impl ButtonStyle {
    fn class(self) -> &'static str {
        match self {
            Self::Primary => "spp-button",
            Self::Ghost => "spp-button spp-button--ghost",
        }
    }
}

/// A button. Use `<Button on_click=.. style=..>"Label"</Button>`.
#[component]
pub fn Button<F>(
    /// Click handler.
    on_click: F,
    /// Visual style. Defaults to Primary.
    #[prop(default = ButtonStyle::default())]
    style: ButtonStyle,
    /// Disabled state.
    #[prop(default = false)]
    disabled: bool,
    /// Optional ARIA label for icon-only buttons.
    #[prop(into, optional)]
    aria_label: Option<String>,
    /// Button content.
    children: Children,
) -> impl IntoView
where
    F: Fn() + 'static,
{
    view! {
        <button
            class=style.class()
            disabled=disabled
            aria-label=aria_label
            on:click=move |_| on_click()
        >
            {children()}
        </button>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_button_class() {
        assert_eq!(ButtonStyle::Primary.class(), "spp-button");
    }

    #[test]
    fn ghost_button_class_includes_modifier() {
        assert_eq!(ButtonStyle::Ghost.class(), "spp-button spp-button--ghost");
    }

    #[test]
    fn default_is_primary() {
        assert_eq!(ButtonStyle::default(), ButtonStyle::Primary);
    }
}
