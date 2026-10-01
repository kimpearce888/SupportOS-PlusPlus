//! App pages (routes). Each page is a Leptos component.
//!
//! Per M1-T08 + M4-T11: routes are `/`, `/operations`, `/notifications`,
//! `/automation`, `/settings`, `/sync-health`, `/inbox`, and a not-found fallback.

pub mod ai_center;
pub mod automation;
pub mod customers;
pub mod dashboard;
pub mod inbox;
pub mod incidents;
pub mod issue_radar;
pub mod knowledge_gaps;
pub mod not_found;
pub mod notifications;
pub mod operations;
pub mod reports;
pub mod settings;
pub mod side_threads;
pub mod support_health;
pub mod sync_health;

pub use ai_center::AiCenterPage;
pub use automation::AutomationPage;
pub use customers::{CustomerProfilePage, CustomerSearchPage};
pub use dashboard::DashboardPage;
pub use inbox::InboxPage;
pub use incidents::IncidentsPage;
pub use issue_radar::IssueRadarPage;
pub use knowledge_gaps::KnowledgeGapsPage;
pub use not_found::NotFoundPage;
pub use notifications::NotificationsPage;
pub use operations::OperationsPage;
pub use reports::ReportsPage;
pub use settings::SettingsPage;
pub use side_threads::SideThreadsPage;
pub use support_health::SupportHealthPage;
pub use sync_health::SyncHealthPage;
