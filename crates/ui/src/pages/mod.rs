//! App pages (routes). Each page is a Leptos component.
//!
//! Per M1-T08 + M4-T02: routes are `/`, `/operations`, `/settings`,
//! `/sync-health`, `/inbox`, and a not-found fallback.

pub mod dashboard;
pub mod inbox;
pub mod not_found;
pub mod operations;
pub mod settings;
pub mod sync_health;

pub use dashboard::DashboardPage;
pub use inbox::InboxPage;
pub use not_found::NotFoundPage;
pub use operations::OperationsPage;
pub use settings::SettingsPage;
pub use sync_health::SyncHealthPage;
