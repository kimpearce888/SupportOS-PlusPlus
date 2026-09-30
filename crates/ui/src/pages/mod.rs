//! App pages (routes). Each page is a Leptos component.
//!
//! Per M1-T08: routes are `/`, `/settings`, `/sync-health`, and a not-found fallback.

pub mod dashboard;
pub mod not_found;
pub mod settings;
pub mod sync_health;

pub use dashboard::DashboardPage;
pub use not_found::NotFoundPage;
pub use settings::SettingsPage;
pub use sync_health::SyncHealthPage;
