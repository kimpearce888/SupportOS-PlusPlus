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

/// A Beacon chat session (M2-T09).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsBeaconChat {
    pub remote_id: i64,
    pub customer_id: i64,
    pub mailbox_id: i64,
    pub status: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// A Docs article (M2-T09).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsDocArticle {
    pub remote_id: i64,
    pub collection_id: i64,
    pub slug: Option<String>,
    pub name: String,
    pub text: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// A CSAT rating (M2-T10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsRating {
    pub remote_id: i64,
    pub conversation_id: i64,
    pub rating: u32,
    pub comment: Option<String>,
    pub created_at: Option<String>,
}

/// A Help Scout folder (per-mailbox view: Unassigned / Mine / Drafts).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsFolder {
    pub remote_id: i64,
    pub mailbox_id: i64,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub user_id: Option<i64>,
    pub total_count: i64,
    pub active_count: i64,
}

/// A Help Scout custom field definition on a mailbox.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsField {
    pub remote_id: i64,
    pub mailbox_id: i64,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub system_type: Option<String>,
    pub required: bool,
    pub sort_order: i64,
    pub options: Vec<HsFieldOption>,
}

/// A dropdown option for a custom field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsFieldOption {
    pub id: i64,
    pub order: i64,
    pub label: String,
}

/// A saved reply macro.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsSavedReply {
    pub remote_id: i64,
    pub name: String,
    pub preview: Option<String>,
    pub text: Option<String>,
}

/// A Help Scout workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsWorkflow {
    pub remote_id: i64,
    pub mailbox_id: Option<i64>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub sort_order: i64,
}

/// A Help Scout webhook configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsWebhookConfig {
    pub remote_id: i64,
    pub url: String,
    pub events: Vec<String>,
    pub status: String,
}

/// A customer/organization property definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsPropertyDef {
    pub remote_id: i64,
    pub name: String,
    pub slug: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub sort_order: i64,
}

/// A Help Scout organization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsOrganization {
    pub remote_id: i64,
    pub name: String,
    pub domains: Vec<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// A conversation thread (message/note/chat line).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsThread {
    pub remote_id: i64,
    pub conversation_id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: Option<String>,
    pub state: Option<String>,
    pub body: Option<String>,
    pub created_by_customer_id: Option<i64>,
    pub created_by_user_id: Option<i64>,
    pub assigned_to_id: Option<i64>,
    pub created_at: Option<String>,
}

/// A user's availability status (email/chat).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsUserStatus {
    pub user_id: i64,
    pub email_status: Option<String>,
    pub email_updated_at: Option<String>,
    pub chat_status: Option<String>,
    pub mailbox_statuses: serde_json::Value,
}

/// A Docs collection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsDocCollection {
    pub remote_id: i64,
    pub slug: Option<String>,
    pub name: String,
}

/// A Docs category within a collection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HsDocCategory {
    pub remote_id: i64,
    pub collection_id: i64,
    pub slug: Option<String>,
    pub name: String,
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

    /// List Beacon chat sessions (M2-T09). Beacon chats live on the same
    /// Help Scout API as conversations but use a different endpoint.
    async fn list_beacon_chats(&self) -> Result<Vec<HsBeaconChat>>;

    /// List Docs articles (M2-T09). Docs use a separate API key
    /// (`docsapi.helpscout.net`) with HTTP Basic auth.
    async fn list_docs(&self) -> Result<Vec<HsDocArticle>>;

    /// List CSAT ratings (M2-T10). Used by the ratings watcher poller.
    async fn list_ratings(&self) -> Result<Vec<HsRating>>;

    // -----------------------------------------------------------------
    // Extended resource surface (reference provider.ts). Default impls
    // keep bounded implementors compiling; the Fake and Real providers
    // override every one.
    // -----------------------------------------------------------------

    /// List per-mailbox folders (Unassigned / Mine / Drafts ...).
    async fn list_folders(&self, _mailbox_id: i64) -> Result<Vec<HsFolder>> {
        Ok(Vec::new())
    }

    /// List custom field definitions for a mailbox.
    async fn list_inbox_fields(&self, _mailbox_id: i64) -> Result<Vec<HsField>> {
        Ok(Vec::new())
    }

    /// List saved replies for a mailbox.
    async fn list_saved_replies(&self, _mailbox_id: i64) -> Result<Vec<HsSavedReply>> {
        Ok(Vec::new())
    }

    /// List workflows.
    async fn list_workflows(&self) -> Result<Vec<HsWorkflow>> {
        Ok(Vec::new())
    }

    /// List registered remote webhooks.
    async fn list_webhooks(&self) -> Result<Vec<HsWebhookConfig>> {
        Ok(Vec::new())
    }

    /// Register a remote webhook; returns its remote id.
    async fn create_webhook(
        &self,
        _url: &str,
        _events: &[String],
        _secret: &str,
        _label: &str,
    ) -> Result<i64> {
        Err(crate::error::Error::Other(
            "webhook registration requires the real provider".into(),
        ))
    }

    /// Delete a remote webhook. Returns false when not found remotely.
    async fn delete_webhook(&self, _remote_id: i64) -> Result<bool> {
        Err(crate::error::Error::Other(
            "webhook deletion requires the real provider".into(),
        ))
    }

    /// List customer property definitions.
    async fn list_customer_property_definitions(&self) -> Result<Vec<HsPropertyDef>> {
        Ok(Vec::new())
    }

    /// List organization property definitions.
    async fn list_organization_property_definitions(&self) -> Result<Vec<HsPropertyDef>> {
        Ok(Vec::new())
    }

    /// List organizations.
    async fn list_organizations(&self) -> Result<Vec<HsOrganization>> {
        Ok(Vec::new())
    }

    /// Fetch a single conversation by remote id (None when 404).
    async fn get_conversation(&self, _conversation_id: i64) -> Result<Option<HsConversation>> {
        Ok(None)
    }

    /// Fetch the threads of a conversation.
    async fn list_threads(&self, _conversation_id: i64) -> Result<Vec<HsThread>> {
        Ok(Vec::new())
    }

    /// Fetch a single customer by remote id (None when 404).
    async fn get_customer(&self, _customer_id: i64) -> Result<Option<HsCustomer>> {
        Ok(None)
    }

    /// Fetch a user's availability status (None when 404).
    async fn get_user_status(&self, _user_id: i64) -> Result<Option<HsUserStatus>> {
        Ok(None)
    }

    /// List system users (automation identities).
    async fn list_system_users(&self) -> Result<Vec<HsUser>> {
        Ok(Vec::new())
    }

    /// List Docs collections.
    async fn list_doc_collections(&self) -> Result<Vec<HsDocCollection>> {
        Ok(Vec::new())
    }

    /// List Docs categories within a collection.
    async fn list_doc_categories(&self, _collection_id: i64) -> Result<Vec<HsDocCategory>> {
        Ok(Vec::new())
    }

    /// List Docs articles within a collection.
    async fn list_doc_articles(&self, _collection_id: i64) -> Result<Vec<HsDocArticle>> {
        Ok(Vec::new())
    }

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
    // Extended resource mirror (reference fakeData.ts shape).
    pub folders: Vec<HsFolder>,
    pub fields: Vec<HsField>,
    pub saved_replies: Vec<HsSavedReply>,
    pub workflows: Vec<HsWorkflow>,
    pub webhooks: Vec<HsWebhookConfig>,
    pub customer_props: Vec<HsPropertyDef>,
    pub org_props: Vec<HsPropertyDef>,
    pub organizations: Vec<HsOrganization>,
    pub threads: Vec<HsThread>,
    pub user_statuses: Vec<HsUserStatus>,
    pub doc_collections: Vec<HsDocCollection>,
    pub doc_categories: Vec<HsDocCategory>,
    pub doc_articles: Vec<HsDocArticle>,
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
            folders: vec![
                HsFolder { remote_id: 501, mailbox_id: 101, name: "Unassigned".into(), kind: "unassigned".into(), user_id: None, total_count: 3, active_count: 2 },
                HsFolder { remote_id: 502, mailbox_id: 101, name: "Mine".into(), kind: "mine".into(), user_id: Some(1), total_count: 4, active_count: 3 },
                HsFolder { remote_id: 503, mailbox_id: 101, name: "Drafts".into(), kind: "drafts".into(), user_id: Some(1), total_count: 1, active_count: 1 },
                HsFolder { remote_id: 504, mailbox_id: 102, name: "Unassigned".into(), kind: "unassigned".into(), user_id: None, total_count: 2, active_count: 1 },
            ],
            fields: vec![
                HsField {
                    remote_id: 104,
                    mailbox_id: 101,
                    name: "Topic".into(),
                    kind: "dropdown".into(),
                    system_type: None,
                    required: false,
                    sort_order: 1,
                    options: vec![
                        HsFieldOption { id: 168, order: 1, label: "Timezone / Scheduling".into() },
                        HsFieldOption { id: 169, order: 2, label: "Registration".into() },
                        HsFieldOption { id: 170, order: 3, label: "Viewer".into() },
                        HsFieldOption { id: 171, order: 4, label: "Integrations".into() },
                        HsFieldOption { id: 172, order: 5, label: "Billing".into() },
                        HsFieldOption { id: 173, order: 6, label: "Automation".into() },
                    ],
                },
                HsField {
                    remote_id: 105,
                    mailbox_id: 101,
                    name: "ai-topic".into(),
                    kind: "dropdown".into(),
                    system_type: Some("topic".into()),
                    required: false,
                    sort_order: 2,
                    options: vec![
                        HsFieldOption { id: 180, order: 1, label: "Billing".into() },
                        HsFieldOption { id: 181, order: 2, label: "Shipping".into() },
                    ],
                },
                HsField { remote_id: 107, mailbox_id: 102, name: "Plan issue".into(), kind: "singleline".into(), system_type: None, required: false, sort_order: 1, options: vec![] },
            ],
            saved_replies: vec![
                HsSavedReply {
                    remote_id: 401,
                    name: "Timezone - set workspace timezone".into(),
                    preview: Some("Hi there! You can change the workspace timezone under Settings > Workspace > Regional...".into()),
                    text: Some("Hi there!\n\nYou can change the workspace timezone under **Settings > Workspace > Regional settings**. After changing it, new scheduled items use the new timezone.".into()),
                },
                HsSavedReply {
                    remote_id: 402,
                    name: "Registration - invite not arriving".into(),
                    preview: Some("Sorry the invite did not arrive. Common causes: spam filtering or a typo in the address...".into()),
                    text: Some("Hi there!\n\nSorry the invite did not arrive. The most common causes are spam filtering or a typo in the address. Could you check your spam folder and confirm the exact address you used?".into()),
                },
                HsSavedReply {
                    remote_id: 403,
                    name: "Billing - update card and retry".into(),
                    preview: Some("You can update your card under Settings > Billing. After updating...".into()),
                    text: Some("Hi there!\n\nYou can update your card under **Settings > Billing > Payment method**. After updating, click Retry payment so the pending invoice is charged again.".into()),
                },
            ],
            workflows: vec![
                HsWorkflow { remote_id: 601, mailbox_id: Some(101), name: "Assign to Tier 1".into(), kind: "manual".into(), status: "active".into(), sort_order: 1 },
                HsWorkflow { remote_id: 602, mailbox_id: Some(101), name: "Spam cleanup".into(), kind: "manual".into(), status: "active".into(), sort_order: 2 },
                HsWorkflow { remote_id: 603, mailbox_id: Some(102), name: "Auto-route billing".into(), kind: "automatic".into(), status: "active".into(), sort_order: 1 },
            ],
            webhooks: vec![
                HsWebhookConfig {
                    remote_id: 801,
                    url: "https://relay.example.com/helpscout".into(),
                    events: vec!["convo.created".into(), "convo.customer.reply.created".into(), "satisfaction.ratings".into()],
                    status: "active".into(),
                },
            ],
            customer_props: vec![
                HsPropertyDef { remote_id: 4101, name: "Plan".into(), slug: Some("plan".into()), kind: "dropdown".into(), sort_order: 1 },
                HsPropertyDef { remote_id: 4102, name: "Employees".into(), slug: Some("employees".into()), kind: "number".into(), sort_order: 2 },
                HsPropertyDef { remote_id: 4103, name: "Region".into(), slug: Some("region".into()), kind: "dropdown".into(), sort_order: 3 },
                HsPropertyDef { remote_id: 4104, name: "Account Manager".into(), slug: Some("account-manager".into()), kind: "text".into(), sort_order: 4 },
            ],
            org_props: vec![
                HsPropertyDef { remote_id: 4201, name: "Industry".into(), slug: Some("industry".into()), kind: "text".into(), sort_order: 1 },
            ],
            organizations: vec![
                HsOrganization { remote_id: 9001, name: "Acme Corp".into(), domains: vec!["acme.com".into()], created_at: Some("2025-06-01T00:00:00Z".into()), updated_at: Some("2026-01-01T00:00:00Z".into()) },
                HsOrganization { remote_id: 9002, name: "Globex".into(), domains: vec!["globex.io".into()], created_at: Some("2025-08-15T00:00:00Z".into()), updated_at: Some("2026-01-01T00:00:00Z".into()) },
            ],
            threads: (1..=10)
                .flat_map(|i| {
                    let conv = 1000 + i;
                    vec![
                        HsThread {
                            remote_id: conv * 10 + 1,
                            conversation_id: conv,
                            kind: "customer".into(),
                            status: Some("active".into()),
                            state: Some("published".into()),
                            body: Some(format!("Customer message for conversation {} — please help with this issue.", i)),
                            created_by_customer_id: Some(2000 + i),
                            created_by_user_id: None,
                            assigned_to_id: None,
                            created_at: Some(format!("2026-01-{:02}T01:00:00Z", i)),
                        },
                        HsThread {
                            remote_id: conv * 10 + 2,
                            conversation_id: conv,
                            kind: "message".into(),
                            status: Some("active".into()),
                            state: Some("published".into()),
                            body: Some(format!("Agent reply for conversation {} — here is what we found.", i)),
                            created_by_customer_id: None,
                            created_by_user_id: Some((i % 3) + 1),
                            assigned_to_id: None,
                            created_at: Some(format!("2026-01-{:02}T05:00:00Z", i)),
                        },
                    ]
                })
                .collect(),
            user_statuses: (1..=3)
                .map(|i| HsUserStatus {
                    user_id: i,
                    email_status: Some(if i == 1 { "away".into() } else { "active".into() }),
                    email_updated_at: Some("2026-01-01T00:00:00Z".into()),
                    chat_status: Some("active".into()),
                    mailbox_statuses: serde_json::json!({ "101": "active", "102": "away" }),
                })
                .collect(),
            doc_collections: vec![
                HsDocCollection { remote_id: 7001, slug: Some("guides".into()), name: "Product Guides".into() },
                HsDocCollection { remote_id: 7002, slug: Some("faq".into()), name: "FAQ".into() },
            ],
            doc_categories: vec![
                HsDocCategory { remote_id: 7101, collection_id: 7001, slug: Some("getting-started".into()), name: "Getting Started".into() },
                HsDocCategory { remote_id: 7102, collection_id: 7001, slug: Some("advanced".into()), name: "Advanced".into() },
                HsDocCategory { remote_id: 7103, collection_id: 7002, slug: Some("billing".into()), name: "Billing FAQ".into() },
            ],
            doc_articles: vec![
                HsDocArticle { remote_id: 7201, collection_id: 7001, slug: Some("create-workspace".into()), name: "Create your first workspace".into(), text: Some("Workspaces hold your dashboards and reports. To create one...".into()), created_at: Some("2025-09-01T00:00:00Z".into()), updated_at: Some("2026-01-01T00:00:00Z".into()) },
                HsDocArticle { remote_id: 7202, collection_id: 7001, slug: Some("invite-teammates".into()), name: "Invite teammates".into(), text: Some("Go to Settings > Members and click Invite...".into()), created_at: Some("2025-09-05T00:00:00Z".into()), updated_at: Some("2026-01-01T00:00:00Z".into()) },
                HsDocArticle { remote_id: 7203, collection_id: 7002, slug: Some("refund-policy".into()), name: "Refund policy".into(), text: Some("Refunds are available within 30 days of purchase...".into()), created_at: Some("2025-10-01T00:00:00Z".into()), updated_at: Some("2026-01-01T00:00:00Z".into()) },
            ],
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

    /// Lock the world mutex (poison-recovering).
    fn lock_world(&self) -> std::sync::MutexGuard<'_, FakeWorld> {
        self.world.lock().unwrap_or_else(|p| p.into_inner())
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
            // 'all' means no status filter (reference fakeProvider parity).
            .filter(|c| {
                query
                    .status
                    .as_deref()
                    .is_none_or(|s| s == "all" || s == c.status)
            })
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

    async fn list_beacon_chats(&self) -> Result<Vec<HsBeaconChat>> {
        // Fake: return 2 demo Beacon chats.
        Ok(vec![
            HsBeaconChat {
                remote_id: 5001,
                customer_id: 2001,
                mailbox_id: 101,
                status: "active".into(),
                created_at: Some("2026-01-15T10:00:00Z".into()),
                updated_at: Some("2026-01-15T10:30:00Z".into()),
            },
            HsBeaconChat {
                remote_id: 5002,
                customer_id: 2002,
                mailbox_id: 102,
                status: "closed".into(),
                created_at: Some("2026-01-16T14:00:00Z".into()),
                updated_at: Some("2026-01-16T14:15:00Z".into()),
            },
        ])
    }

    async fn list_docs(&self) -> Result<Vec<HsDocArticle>> {
        // Fake: return 3 demo Docs articles.
        Ok(vec![
            HsDocArticle {
                remote_id: 6001,
                collection_id: 101,
                slug: Some("getting-started".into()),
                name: "Getting Started Guide".into(),
                text: Some("Welcome to SupportOS++! This guide covers the basics.".into()),
                created_at: Some("2026-01-01T00:00:00Z".into()),
                updated_at: Some("2026-01-10T12:00:00Z".into()),
            },
            HsDocArticle {
                remote_id: 6002,
                collection_id: 101,
                slug: Some("faq".into()),
                name: "FAQ".into(),
                text: Some("Frequently asked questions about SupportOS++.".into()),
                created_at: Some("2026-01-05T00:00:00Z".into()),
                updated_at: Some("2026-01-12T09:00:00Z".into()),
            },
            HsDocArticle {
                remote_id: 6003,
                collection_id: 102,
                slug: Some("troubleshooting".into()),
                name: "Troubleshooting".into(),
                text: Some("Common issues and how to resolve them.".into()),
                created_at: Some("2026-01-08T00:00:00Z".into()),
                updated_at: Some("2026-01-14T16:00:00Z".into()),
            },
        ])
    }

    async fn list_ratings(&self) -> Result<Vec<HsRating>> {
        // Fake: return 3 demo ratings (mix of 5-star and 3-star).
        Ok(vec![
            HsRating {
                remote_id: 7001,
                conversation_id: 1001,
                rating: 5,
                comment: Some("Great support!".into()),
                created_at: Some("2026-01-10T12:00:00Z".into()),
            },
            HsRating {
                remote_id: 7002,
                conversation_id: 1002,
                rating: 3,
                comment: Some("It was okay.".into()),
                created_at: Some("2026-01-11T15:00:00Z".into()),
            },
            HsRating {
                remote_id: 7003,
                conversation_id: 1004,
                rating: 5,
                comment: None,
                created_at: Some("2026-01-12T09:00:00Z".into()),
            },
        ])
    }

    fn reset(&self) {
        let mut world = self
            .world
            .lock()
            .expect("FakeHelpScoutProvider mutex poisoned");
        *world = FakeWorld::demo();
    }

    // ---------------- Extended resource surface ----------------

    async fn list_folders(&self, mailbox_id: i64) -> Result<Vec<HsFolder>> {
        let world = self.lock_world();
        Ok(world
            .folders
            .iter()
            .filter(|f| f.mailbox_id == mailbox_id)
            .cloned()
            .collect())
    }

    async fn list_inbox_fields(&self, mailbox_id: i64) -> Result<Vec<HsField>> {
        let world = self.lock_world();
        Ok(world
            .fields
            .iter()
            .filter(|f| f.mailbox_id == mailbox_id)
            .cloned()
            .collect())
    }

    async fn list_saved_replies(&self, _mailbox_id: i64) -> Result<Vec<HsSavedReply>> {
        let world = self.lock_world();
        Ok(world.saved_replies.clone())
    }

    async fn list_workflows(&self) -> Result<Vec<HsWorkflow>> {
        let world = self.lock_world();
        Ok(world.workflows.clone())
    }

    async fn list_webhooks(&self) -> Result<Vec<HsWebhookConfig>> {
        let world = self.lock_world();
        Ok(world.webhooks.clone())
    }

    async fn create_webhook(
        &self,
        url: &str,
        events: &[String],
        _secret: &str,
        _label: &str,
    ) -> Result<i64> {
        let mut world = self.lock_world();
        let next_id = world
            .webhooks
            .iter()
            .map(|w| w.remote_id)
            .max()
            .unwrap_or(0)
            + 1;
        world.webhooks.push(HsWebhookConfig {
            remote_id: next_id,
            url: url.to_string(),
            events: events.to_vec(),
            status: "enabled".into(),
        });
        Ok(next_id)
    }

    async fn delete_webhook(&self, remote_id: i64) -> Result<bool> {
        let mut world = self.lock_world();
        let before = world.webhooks.len();
        world.webhooks.retain(|w| w.remote_id != remote_id);
        Ok(world.webhooks.len() < before)
    }

    async fn list_customer_property_definitions(&self) -> Result<Vec<HsPropertyDef>> {
        let world = self.lock_world();
        Ok(world.customer_props.clone())
    }

    async fn list_organization_property_definitions(&self) -> Result<Vec<HsPropertyDef>> {
        let world = self.lock_world();
        Ok(world.org_props.clone())
    }

    async fn list_organizations(&self) -> Result<Vec<HsOrganization>> {
        let world = self.lock_world();
        Ok(world.organizations.clone())
    }

    async fn get_conversation(&self, conversation_id: i64) -> Result<Option<HsConversation>> {
        let world = self.lock_world();
        Ok(world
            .conversations
            .iter()
            .find(|c| c.remote_id == conversation_id)
            .cloned())
    }

    async fn list_threads(&self, conversation_id: i64) -> Result<Vec<HsThread>> {
        let world = self.lock_world();
        let mut threads: Vec<HsThread> = world
            .threads
            .iter()
            .filter(|t| t.conversation_id == conversation_id)
            .cloned()
            .collect();
        threads.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(threads)
    }

    async fn get_customer(&self, customer_id: i64) -> Result<Option<HsCustomer>> {
        let world = self.lock_world();
        Ok(world
            .customers
            .iter()
            .find(|c| c.remote_id == customer_id)
            .cloned())
    }

    async fn get_user_status(&self, user_id: i64) -> Result<Option<HsUserStatus>> {
        let world = self.lock_world();
        Ok(world
            .user_statuses
            .iter()
            .find(|s| s.user_id == user_id)
            .cloned())
    }

    async fn list_system_users(&self) -> Result<Vec<HsUser>> {
        Ok(Vec::new())
    }

    async fn list_doc_collections(&self) -> Result<Vec<HsDocCollection>> {
        let world = self.lock_world();
        Ok(world.doc_collections.clone())
    }

    async fn list_doc_categories(&self, collection_id: i64) -> Result<Vec<HsDocCategory>> {
        let world = self.lock_world();
        Ok(world
            .doc_categories
            .iter()
            .filter(|c| c.collection_id == collection_id)
            .cloned()
            .collect())
    }

    async fn list_doc_articles(&self, collection_id: i64) -> Result<Vec<HsDocArticle>> {
        let world = self.lock_world();
        Ok(world
            .doc_articles
            .iter()
            .filter(|a| a.collection_id == collection_id)
            .cloned()
            .collect())
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

    #[tokio::test]
    async fn list_beacon_chats_returns_two() {
        let p = provider();
        let chats = p.list_beacon_chats().await.unwrap();
        assert_eq!(chats.len(), 2);
        assert_eq!(chats[0].customer_id, 2001);
        assert_eq!(chats[1].status, "closed");
    }

    #[tokio::test]
    async fn list_docs_returns_three() {
        let p = provider();
        let docs = p.list_docs().await.unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0].name, "Getting Started Guide");
        assert!(docs[1].slug.as_ref().is_some_and(|s| s == "faq"));
    }

    #[tokio::test]
    async fn list_ratings_returns_three() {
        let p = provider();
        let ratings = p.list_ratings().await.unwrap();
        assert_eq!(ratings.len(), 3);
        assert_eq!(ratings[0].rating, 5);
        assert_eq!(ratings[1].rating, 3);
        assert!(ratings[2].comment.is_none());
    }
}
