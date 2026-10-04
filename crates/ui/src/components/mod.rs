//! Common UI components — shared foundation used by every view.
//!
//! Per spec KNOWN PITFALLS: every view has loading, empty, and error states.
//! Per A12: one source of truth per pattern — these components are that source.

pub mod attribute_snapshot;
pub mod button;
pub mod coaching_panel;
pub mod command_palette;
pub mod copilot_panel;
pub mod memory_panel;
pub mod mention;
pub mod mention_textarea;
pub mod onboarding;
pub mod overlays;
pub mod qa_panel;
pub mod side_threads;
pub mod state_view;
pub mod theming;
pub mod translation_panel;

pub use attribute_snapshot::AttributeSnapshotCard;
pub use button::Button;
pub use coaching_panel::CoachingPanel;
pub use command_palette::CommandPalette;
pub use copilot_panel::CopilotPanel;
pub use memory_panel::MemoryPanel;
pub use mention_textarea::MentionTextarea;
pub use onboarding::{OnboardingOverlay, OnboardingProps};
pub use overlays::ConfirmDialog;
pub use qa_panel::QaPanel;
pub use side_threads::SideThreadsPanel;
pub use state_view::{EmptyState, ErrorState, LoadingState, StateView, ViewState};
pub use translation_panel::TranslationPanel;
