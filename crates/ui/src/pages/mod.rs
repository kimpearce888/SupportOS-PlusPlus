//! App pages (routes). Each page is a Leptos component.
//!
//! Per M1-T08: routes are `/`, `/settings`, and a not-found fallback.

pub mod dashboard;
pub mod not_found;
pub mod settings;

pub use dashboard::DashboardPage;
pub use not_found::NotFoundPage;
pub use settings::SettingsPage;
