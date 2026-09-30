//! Common UI components — shared foundation used by every view.
//!
//! Per spec KNOWN PITFALLS: every view has loading, empty, and error states.
//! Per A12: one source of truth per pattern — these components are that source.

pub mod button;
pub mod command_palette;
pub mod onboarding;
pub mod state_view;
pub mod theming;

pub use button::Button;
pub use command_palette::CommandPalette;
pub use onboarding::{OnboardingOverlay, OnboardingProps};
pub use state_view::{EmptyState, ErrorState, LoadingState, StateView, ViewState};
