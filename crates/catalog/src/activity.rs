//! Activity catalog — activity fields, date modes, response states, condition kinds.
//!
//! Verified counts (session 1, see `docs/PARITY-MATRIX.md`):
//! - 14 activity fields
//! - 15 date modes (7 calendar + 6 rolling + 2 exact)
//! - 22 saved-view condition kinds
//! - response states (counted from reference's `RESPONSE_STATES` array)

use serde::{Deserialize, Serialize};

/// One of the 14 activity fields a saved-view condition can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityField {
    CreatedAt,
    FirstCustomerMessageAt,
    FirstResponseAt,
    LastCustomerReplyAt,
    LastHumanAgentResponseAt,
    LastSystemResponseAt,
    LastNoteAt,
    LastActivityAt,
    ClosedAt,
    CustomerWaitingSince,
    LastStatusChangeAt,
    LastAssignmentChangeAt,
    LastTagChangeAt,
    LastCustomFieldChangeAt,
}

impl ActivityField {
    /// All variants in spec order.
    pub const ALL: [Self; 14] = [
        Self::CreatedAt,
        Self::FirstCustomerMessageAt,
        Self::FirstResponseAt,
        Self::LastCustomerReplyAt,
        Self::LastHumanAgentResponseAt,
        Self::LastSystemResponseAt,
        Self::LastNoteAt,
        Self::LastActivityAt,
        Self::ClosedAt,
        Self::CustomerWaitingSince,
        Self::LastStatusChangeAt,
        Self::LastAssignmentChangeAt,
        Self::LastTagChangeAt,
        Self::LastCustomFieldChangeAt,
    ];

    /// Convert to the snake_case string stored in the DB.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at",
            Self::FirstCustomerMessageAt => "first_customer_message_at",
            Self::FirstResponseAt => "first_response_at",
            Self::LastCustomerReplyAt => "last_customer_reply_at",
            Self::LastHumanAgentResponseAt => "last_human_agent_response_at",
            Self::LastSystemResponseAt => "last_system_response_at",
            Self::LastNoteAt => "last_note_at",
            Self::LastActivityAt => "last_activity_at",
            Self::ClosedAt => "closed_at",
            Self::CustomerWaitingSince => "customer_waiting_since",
            Self::LastStatusChangeAt => "last_status_change_at",
            Self::LastAssignmentChangeAt => "last_assignment_change_at",
            Self::LastTagChangeAt => "last_tag_change_at",
            Self::LastCustomFieldChangeAt => "last_custom_field_change_at",
        }
    }
}

/// One of the 15 date modes a saved-view date condition can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateMode {
    // 7 calendar modes
    Today,
    Yesterday,
    Tomorrow,
    ThisWeek,
    LastWeek,
    ThisMonth,
    LastMonth,
    // 6 rolling modes
    Last24h,
    Last48h,
    Last7d,
    Last14d,
    Last30d,
    Last90d,
    // 2 exact modes
    ExactDate,
    CustomRange,
}

impl DateMode {
    /// All 15 variants in spec order.
    pub const ALL: [Self; 15] = [
        Self::Today,
        Self::Yesterday,
        Self::Tomorrow,
        Self::ThisWeek,
        Self::LastWeek,
        Self::ThisMonth,
        Self::LastMonth,
        Self::Last24h,
        Self::Last48h,
        Self::Last7d,
        Self::Last14d,
        Self::Last30d,
        Self::Last90d,
        Self::ExactDate,
        Self::CustomRange,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::Tomorrow => "tomorrow",
            Self::ThisWeek => "this_week",
            Self::LastWeek => "last_week",
            Self::ThisMonth => "this_month",
            Self::LastMonth => "last_month",
            Self::Last24h => "last_24h",
            Self::Last48h => "last_48h",
            Self::Last7d => "last_7d",
            Self::Last14d => "last_14d",
            Self::Last30d => "last_30d",
            Self::Last90d => "last_90d",
            Self::ExactDate => "exact_date",
            Self::CustomRange => "custom_range",
        }
    }
}

/// One of the 22 saved-view condition kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionKind {
    Status,
    Assignee,
    Team,
    Mailbox,
    Channel,
    Tags,
    CustomField,
    CustomerProperty,
    CustomerText,
    DateActivity,
    ResponseState,
    ResponseAge,
    Sla,
    Priority,
    TicketState,
    KnownIssue,
    AiAnalyzed,
    InteractionSignal,
    AiAttribute,
    Unread,
    Snoozed,
    Customer,
}

impl ConditionKind {
    /// All 22 variants in spec order.
    pub const ALL: [Self; 22] = [
        Self::Status,
        Self::Assignee,
        Self::Team,
        Self::Mailbox,
        Self::Channel,
        Self::Tags,
        Self::CustomField,
        Self::CustomerProperty,
        Self::CustomerText,
        Self::DateActivity,
        Self::ResponseState,
        Self::ResponseAge,
        Self::Sla,
        Self::Priority,
        Self::TicketState,
        Self::KnownIssue,
        Self::AiAnalyzed,
        Self::InteractionSignal,
        Self::AiAttribute,
        Self::Unread,
        Self::Snoozed,
        Self::Customer,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Assignee => "assignee",
            Self::Team => "team",
            Self::Mailbox => "mailbox",
            Self::Channel => "channel",
            Self::Tags => "tags",
            Self::CustomField => "custom_field",
            Self::CustomerProperty => "customer_property",
            Self::CustomerText => "customer_text",
            Self::DateActivity => "date_activity",
            Self::ResponseState => "response_state",
            Self::ResponseAge => "response_age",
            Self::Sla => "sla",
            Self::Priority => "priority",
            Self::TicketState => "ticket_state",
            Self::KnownIssue => "known_issue",
            Self::AiAnalyzed => "ai_analyzed",
            Self::InteractionSignal => "interaction_signal",
            Self::AiAttribute => "ai_attribute",
            Self::Unread => "unread",
            Self::Snoozed => "snoozed",
            Self::Customer => "customer",
        }
    }
}

/// Response state of a conversation — derived, single source of truth for both the
/// Operations Center tiles and inbox filters (KNOWN PITFALL: same fragment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseState {
    NeedsFirstResponse,
    CustomerWaiting,
    AgentWaiting,
    Closed,
}

impl ResponseState {
    pub const ALL: [Self; 4] = [
        Self::NeedsFirstResponse,
        Self::CustomerWaiting,
        Self::AgentWaiting,
        Self::Closed,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsFirstResponse => "needs_first_response",
            Self::CustomerWaiting => "customer_waiting",
            Self::AgentWaiting => "agent_waiting",
            Self::Closed => "closed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_field_count_matches_spec() {
        assert_eq!(
            ActivityField::ALL.len(),
            14,
            "spec mandates 14 activity fields"
        );
    }

    #[test]
    fn date_mode_count_matches_spec() {
        assert_eq!(DateMode::ALL.len(), 15, "spec mandates 15 date modes");
    }

    #[test]
    fn condition_kind_count_matches_spec() {
        assert_eq!(
            ConditionKind::ALL.len(),
            22,
            "spec mandates 22 condition kinds"
        );
    }

    #[test]
    fn snake_case_round_trip() {
        for f in ActivityField::ALL {
            let s = serde_json::to_string(&f).unwrap();
            let back: ActivityField = serde_json::from_str(&s).unwrap();
            assert_eq!(f, back);
            assert!(s.contains(f.as_str()));
        }
    }
}
