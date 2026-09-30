//! HelpScoutProvider trait — the integration boundary for Help Scout.
//!
//! Per spec A12: "Put each concern behind a small trait or module with a narrow
//! public surface." The whole sync/UI layer never sees endpoint URLs or API
//! versions — only the normalized DTOs defined here.
//!
//! Two implementations (M2):
//! - `FakeHelpScoutProvider`: in-memory, mutable simulator for demo mode + tests.
//! - `RealHelpScoutProvider`: HTTP client using the OAuth token (lands with M2-T02).
//!
//! The trait is `async` because the Real implementation does network I/O. The
//! Fake implementation returns immediately from memory.

use serde::{Deserialize, Serialize};

use crate::error::Result;

// ---------------------------------------------------------------------------
// Normalized DTOs (version-neutral)
// ---------------------------------------------------------------------------

/// A Help Scout user (agent or system user).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct HsUser {
    pub remote_id: i64,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub role: Option<String>,
    /// "user" or "system_user".
    pub user_type: String,
    pub timezone: Option<String>,
    pub photo_url: Option<String>,
    pub initials: Option<String>,
    pub mention: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
    pub alternate_emails: Vec<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// A Help Scout team.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsTeam {
    pub remote_id: i64,
    pub name: String,
    pub member_user_ids: Vec<i64>,
}

/// A Help Scout mailbox.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsMailbox {
    pub remote_id: i64,
    pub name: String,
    pub slug: Option<String>,
    pub email: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// A Help Scout tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsTag {
    pub remote_id: i64,
    pub name: String,
    pub slug: Option<String>,
    pub color: Option<String>,
    pub ticket_count: Option<i64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// A Help Scout customer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsCustomer {
    pub remote_id: i64,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// A Help Scout conversation (ticket).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsConversation {
    pub remote_id: i64,
    pub number: i64,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub status: String,
    pub mailbox_id: i64,
    pub assignee_id: Option<i64>,
    pub customer_id: i64,
    pub priority: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
}

/// A paginated response page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// The cursor for the next page; `None` if this is the last page.
    pub next_cursor: Option<String>,
}

/// Query parameters for listing conversations.
#[derive(Debug, Clone, Default)]
pub struct ConversationQuery {
    pub mailbox_id: Option<i64>,
    pub status: Option<String>,
    pub modified_since: Option<String>,
    pub cursor: Option<String>,
    pub page_size: Option<u32>,
}

/// Query parameters for listing customers.
#[derive(Debug, Clone, Default)]
pub struct CustomerQuery {
    pub mailbox_id: Option<i64>,
    pub modified_since: Option<String>,
    pub cursor: Option<String>,
    pub page_size: Option<u32>,
}

// ---------------------------------------------------------------------------
// The provider trait
// ---------------------------------------------------------------------------

/// The Help Scout integration boundary. Both Real and Fake implementations
/// implement this trait.
///
/// Methods are `async` because the Real implementation does HTTP I/O.
/// The Fake implementation returns immediately from memory.
#[async_trait::async_trait]
pub trait HelpScoutProvider: Send + Sync {
    /// "fake" or "real". Used for diagnostics + demo-mode enforcement.
    fn kind(&self) -> &'static str;

    /// Get the currently authenticated user (the "me" endpoint).
    async fn get_me(&self) -> Result<HsUser>;

    /// List mailboxes.
    async fn list_mailboxes(&self) -> Result<Vec<HsMailbox>>;

    /// List users (agents + system users).
    async fn list_users(&self) -> Result<Vec<HsUser>>;

    /// List teams.
    async fn list_teams(&self) -> Result<Vec<HsTeam>>;

    /// List tags.
    async fn list_tags(&self) -> Result<Vec<HsTag>>;

    /// List conversations with pagination + filtering.
    async fn list_conversations(&self, query: &ConversationQuery) -> Result<Page<HsConversation>>;

    /// List customers with pagination + filtering.
    async fn list_customers(&self, query: &CustomerQuery) -> Result<Page<HsCustomer>>;

    /// Reset the provider's state (Fake only; Real is a no-op).
    /// Used by tests to get a clean slate.
    fn reset(&self) {}
}

// ---------------------------------------------------------------------------
// FakeHelpScoutProvider (demo mode + tests)
// ---------------------------------------------------------------------------

use std::sync::Mutex;

/// An in-memory, mutable Help Scout simulator. Used for demo mode
/// (`demo_mode = true` in settings) and for the entire automated test suite.
pub struct FakeHelpScoutProvider {
    world: Mutex<FakeWorld>,
}

/// The in-memory data store for the Fake provider.
#[derive(Debug, Clone, Default)]
pub struct FakeWorld {
    pub me: HsUser,
    pub mailboxes: Vec<HsMailbox>,
    pub users: Vec<HsUser>,
    pub teams: Vec<HsTeam>,
    pub tags: Vec<HsTag>,
    pub conversations: Vec<HsConversation>,
    pub customers: Vec<HsCustomer>,
}

impl FakeWorld {
    /// Build a deterministic demo world with a small set of sample data.
    /// The data is designed to exercise the UI: 2 mailboxes, 3 agents,
    /// 2 teams, 5 tags, 10 conversations, 8 customers.
    #[must_use]
    pub fn demo() -> Self {
        Self {
            me: HsUser {
                remote_id: 1,
                first_name: Some("Demo".into()),
                last_name: Some("Agent".into()),
                email: Some("demo@supportos.test".into()),
                role: Some("owner".into()),
                user_type: "user".into(),
                timezone: Some("UTC".into()),
                photo_url: None,
                initials: Some("DA".into()),
                mention: Some("@demo".into()),
                job_title: Some("Support Lead".into()),
                phone: None,
                alternate_emails: vec![],
                created_at: Some("2026-01-01T00:00:00Z".into()),
                updated_at: Some("2026-01-01T00:00:00Z".into()),
            },
            mailboxes: vec![
                HsMailbox {
                    remote_id: 101,
                    name: "General Support".into(),
                    slug: Some("general".into()),
                    email: Some("support@example.com".into()),
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
                HsMailbox {
                    remote_id: 102,
                    name: "Billing".into(),
                    slug: Some("billing".into()),
                    email: Some("billing@example.com".into()),
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
            ],
            users: vec![
                HsUser {
                    remote_id: 1,
                    first_name: Some("Demo".into()),
                    last_name: Some("Agent".into()),
                    email: Some("demo@supportos.test".into()),
                    role: Some("owner".into()),
                    user_type: "user".into(),
                    timezone: Some("UTC".into()),
                    photo_url: None,
                    initials: Some("DA".into()),
                    mention: Some("@demo".into()),
                    job_title: Some("Support Lead".into()),
                    phone: None,
                    alternate_emails: vec![],
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
                HsUser {
                    remote_id: 2,
                    first_name: Some("Jane".into()),
                    last_name: Some("Smith".into()),
                    email: Some("jane@supportos.test".into()),
                    role: Some("admin".into()),
                    user_type: "user".into(),
                    timezone: Some("UTC".into()),
                    photo_url: None,
                    initials: Some("JS".into()),
                    mention: Some("@jane".into()),
                    job_title: Some("Agent".into()),
                    phone: None,
                    alternate_emails: vec![],
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
                HsUser {
                    remote_id: 3,
                    first_name: Some("Bob".into()),
                    last_name: Some("Jones".into()),
                    email: Some("bob@supportos.test".into()),
                    role: Some("user".into()),
                    user_type: "user".into(),
                    timezone: Some("UTC".into()),
                    photo_url: None,
                    initials: Some("BJ".into()),
                    mention: Some("@bob".into()),
                    job_title: Some("Agent".into()),
                    phone: None,
                    alternate_emails: vec![],
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
            ],
            teams: vec![
                HsTeam {
                    remote_id: 201,
                    name: "Support Team".into(),
                    member_user_ids: vec![1, 2, 3],
                },
                HsTeam {
                    remote_id: 202,
                    name: "Billing Team".into(),
                    member_user_ids: vec![2, 3],
                },
            ],
            tags: vec![
                HsTag {
                    remote_id: 301,
                    name: "bug".into(),
                    slug: Some("bug".into()),
                    color: Some("#f85149".into()),
                    ticket_count: Some(3),
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
                HsTag {
                    remote_id: 302,
                    name: "feature-request".into(),
                    slug: Some("feature-request".into()),
                    color: Some("#4f9cf9".into()),
                    ticket_count: Some(2),
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
                HsTag {
                    remote_id: 303,
                    name: "urgent".into(),
                    slug: Some("urgent".into()),
                    color: Some("#d29922".into()),
                    ticket_count: Some(1),
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
                HsTag {
                    remote_id: 304,
                    name: "billing".into(),
                    slug: Some("billing".into()),
                    color: Some("#3fb950".into()),
                    ticket_count: Some(4),
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
                HsTag {
                    remote_id: 305,
                    name: "how-to".into(),
                    slug: Some("how-to".into()),
                    color: Some("#8b949e".into()),
                    ticket_count: Some(5),
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                },
            ],
            conversations: (1..=10)
                .map(|i| HsConversation {
                    remote_id: 1000 + i,
                    number: 1000 + i,
                    subject: Some(format!("Conversation #{}", i)),
                    preview: Some(format!("Preview text for conversation {}", i)),
                    status: if i % 3 == 0 { "closed" } else { "active" }.into(),
                    mailbox_id: if i <= 5 { 101 } else { 102 },
                    assignee_id: Some((i % 3) + 1),
                    customer_id: 2000 + i,
                    priority: if i == 1 { Some("urgent".into()) } else { None },
                    created_at: Some(format!("2026-01-{:02}T00:00:00Z", i)),
                    updated_at: Some(format!("2026-01-{:02}T12:00:00Z", i)),
                    closed_at: if i % 3 == 0 {
                        Some(format!("2026-01-{:02}T18:00:00Z", i))
                    } else {
                        None
                    },
                })
                .collect(),
            customers: (1..=8)
                .map(|i| HsCustomer {
                    remote_id: 2000 + i,
                    first_name: Some(format!("Customer{}", i)),
                    last_name: Some("Last".into()),
                    email: Some(format!("customer{}@example.com", i)),
                    organization: Some(if i <= 4 { "Acme Corp" } else { "Globex" }.into()),
                    job_title: Some(if i <= 4 { "Engineer" } else { "Manager" }.into()),
                    phone: Some(format!("+1-555-{:04}", i)),
                    created_at: Some("2026-01-01T00:00:00Z".into()),
                    updated_at: Some("2026-01-01T00:00:00Z".into()),
                })
                .collect(),
        }
    }
}

impl FakeHelpScoutProvider {
    /// Create a Fake provider with the deterministic demo world.
    #[must_use]
    pub fn new_demo() -> Self {
        Self {
            world: Mutex::new(FakeWorld::demo()),
        }
    }

    /// Create a Fake provider with an empty world (for tests).
    #[must_use]
    pub fn new_empty() -> Self {
        Self {
            world: Mutex::new(FakeWorld::default()),
        }
    }
}

#[async_trait::async_trait]
impl HelpScoutProvider for FakeHelpScoutProvider {
    fn kind(&self) -> &'static str {
        "fake"
    }

    async fn get_me(&self) -> Result<HsUser> {
        let world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        Ok(world.me.clone())
    }

    async fn list_mailboxes(&self) -> Result<Vec<HsMailbox>> {
        let world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        Ok(world.mailboxes.clone())
    }

    async fn list_users(&self) -> Result<Vec<HsUser>> {
        let world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        Ok(world.users.clone())
    }

    async fn list_teams(&self) -> Result<Vec<HsTeam>> {
        let world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        Ok(world.teams.clone())
    }

    async fn list_tags(&self) -> Result<Vec<HsTag>> {
        let world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        Ok(world.tags.clone())
    }

    async fn list_conversations(&self, query: &ConversationQuery) -> Result<Page<HsConversation>> {
        let world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        let mut items: Vec<HsConversation> = world
            .conversations
            .iter()
            .filter(|c| query.mailbox_id.is_none_or(|m| c.mailbox_id == m))
            .filter(|c| query.status.as_ref().is_none_or(|s| &c.status == s))
            .cloned()
            .collect();
        #[allow(clippy::cast_possible_truncation)]
        let page_size = query.page_size.unwrap_or(50) as usize;
        let take = page_size.min(items.len());
        let _remaining = items.split_off(take);
        Ok(Page {
            items,
            next_cursor: None, // Fake has no pagination — all on one page
        })
    }

    async fn list_customers(&self, query: &CustomerQuery) -> Result<Page<HsCustomer>> {
        let world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        let items = world.customers.clone();
        let _ = query; // Fake doesn't filter customers (M2 will add if needed)
        Ok(Page {
            items,
            next_cursor: None,
        })
    }

    fn reset(&self) {
        let mut world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        *world = FakeWorld::demo();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> FakeHelpScoutProvider {
        FakeHelpScoutProvider::new_demo()
    }

    #[tokio::test]
    async fn get_me_returns_demo_agent() {
        let p = provider();
        let me = p.get_me().await.unwrap();
        assert_eq!(me.first_name, Some("Demo".into()));
        assert_eq!(me.email, Some("demo@supportos.test".into()));
    }

    #[tokio::test]
    async fn list_mailboxes_returns_two() {
        let p = provider();
        let mailboxes = p.list_mailboxes().await.unwrap();
        assert_eq!(mailboxes.len(), 2);
        assert_eq!(mailboxes[0].name, "General Support");
        assert_eq!(mailboxes[1].name, "Billing");
    }

    #[tokio::test]
    async fn list_users_returns_three() {
        let p = provider();
        let users = p.list_users().await.unwrap();
        assert_eq!(users.len(), 3);
    }

    #[tokio::test]
    async fn list_teams_returns_two() {
        let p = provider();
        let teams = p.list_teams().await.unwrap();
        assert_eq!(teams.len(), 2);
        assert_eq!(teams[0].member_user_ids.len(), 3);
    }

    #[tokio::test]
    async fn list_tags_returns_five() {
        let p = provider();
        let tags = p.list_tags().await.unwrap();
        assert_eq!(tags.len(), 5);
    }

    #[tokio::test]
    async fn list_conversations_returns_ten() {
        let p = provider();
        let page = p
            .list_conversations(&ConversationQuery::default())
            .await
            .unwrap();
        assert_eq!(page.items.len(), 10);
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn list_conversations_filters_by_mailbox() {
        let p = provider();
        let page = p
            .list_conversations(&ConversationQuery {
                mailbox_id: Some(101),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 5);
        assert!(page.items.iter().all(|c| c.mailbox_id == 101));
    }

    #[tokio::test]
    async fn list_conversations_filters_by_status() {
        let p = provider();
        let page = p
            .list_conversations(&ConversationQuery {
                status: Some("closed".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 3); // items 3, 6, 9 are closed
        assert!(page.items.iter().all(|c| c.status == "closed"));
    }

    #[tokio::test]
    async fn list_customers_returns_eight() {
        let p = provider();
        let page = p.list_customers(&CustomerQuery::default()).await.unwrap();
        assert_eq!(page.items.len(), 8);
    }

    #[tokio::test]
    async fn reset_restores_demo_world() {
        let p = provider();
        // The demo world has 10 conversations; after reset it should still be 10.
        let before = p
            .list_conversations(&ConversationQuery::default())
            .await
            .unwrap();
        assert_eq!(before.items.len(), 10);
        p.reset();
        let after = p
            .list_conversations(&ConversationQuery::default())
            .await
            .unwrap();
        assert_eq!(after.items.len(), 10);
    }

    #[test]
    fn fake_world_demo_is_deterministic() {
        let w1 = FakeWorld::demo();
        let w2 = FakeWorld::demo();
        assert_eq!(w1.mailboxes.len(), w2.mailboxes.len());
        assert_eq!(w1.conversations.len(), w2.conversations.len());
        assert_eq!(w1.conversations[0].subject, w2.conversations[0].subject);
    }

    #[test]
    fn kind_is_fake() {
        let p = provider();
        assert_eq!(p.kind(), "fake");
    }
}
