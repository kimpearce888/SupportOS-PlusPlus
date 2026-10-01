//! App pages (routes). Each page is a Leptos component.
//!
//! Per M1-T08 + M4-T11: routes are `/`, `/operations`, `/notifications`,
//! `/automation`, `/settings`, `/sync-health`, `/inbox`, and a not-found fallback.

pub mod ai_center;
pub mod automation;
pub mod customers;
pub mod dashboard;
pub mod inbox;
pub mod not_found;
pub mod notifications;
pub mod operations;
pub mod settings;
pub mod sync_health;

pub use ai_center::AiCenterPage;
pub use automation::AutomationPage;
pub use customers::{CustomerProfilePage, CustomerSearchPage};
pub use dashboard::DashboardPage;
pub use inbox::InboxPage;
pub use not_found::NotFoundPage;
pub use notifications::NotificationsPage;
pub use operations::OperationsPage;
pub use settings::SettingsPage;
pub use sync_health::SyncHealthPage;
