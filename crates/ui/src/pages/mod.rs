//! App pages (routes). Each page is a Leptos component.
//!
//! Per M1-T08 + M4-T11: routes are `/`, `/operations`, `/notifications`,
//! `/automation`, `/settings`, `/sync-health`, `/inbox`, and a not-found fallback.

pub mod ai_center;
pub mod automation;
pub mod backup;
pub mod command_palette;
pub mod connectors;
pub mod custom_objects;
pub mod customers;
pub mod dashboard;
pub mod inbox;
pub mod incidents;
pub mod issue_radar;
pub mod knowledge_gaps;
pub mod not_found;
pub mod notifications;
pub mod onboarding;
pub mod operations;
pub mod outreach;
pub mod reports;
pub mod search;
pub mod settings;
pub mod side_threads;
pub mod support_graph;
pub mod support_health;
pub mod sync_health;

pub use ai_center::AiCenterPage;
pub use automation::AutomationPage;
pub use backup::BackupPage;
pub use command_palette::CommandPalettePage;
pub use connectors::ConnectorsPage;
pub use custom_objects::CustomObjectsPage;
pub use customers::{CustomerProfilePage, CustomerSearchPage};
pub use dashboard::DashboardPage;
pub use inbox::InboxPage;
pub use incidents::IncidentsPage;
pub use issue_radar::IssueRadarPage;
pub use knowledge_gaps::KnowledgeGapsPage;
pub use not_found::NotFoundPage;
pub use notifications::NotificationsPage;
pub use onboarding::OnboardingPage;
pub use operations::OperationsPage;
pub use outreach::OutreachPage;
pub use reports::ReportsPage;
pub use search::SearchPage;
pub use settings::SettingsPage;
pub use side_threads::SideThreadsPage;
pub use support_graph::SupportGraphPage;
pub use support_health::SupportHealthPage;
pub use sync_health::SyncHealthPage;
