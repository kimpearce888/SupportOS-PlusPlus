//! Report metrics (21) and dimensions (14) for the custom report builder.

use serde::{Deserialize, Serialize};

/// One of the 21 report metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportMetricKey {
    Conversations,
    UniqueCustomers,
    Organizations,
    FirstResponses,
    AgentReplies,
    CustomerReplies,
    Closures,
    AvgFirstResponseMinutes,
    AvgResolutionMinutes,
    AvgWaitHours,
    SlaBreached,
    StateChanges,
    AvgStateHours,
    HighPriorityRate,
    IssueLinkedShare,
    AiAttributeShare,
    AvgCustomerEffort,
    HighFrictionRate,
    CampaignSent,
    CampaignReplies,
    CampaignReplyRate,
}

impl ReportMetricKey {
    pub const ALL: [Self; 21] = [
        Self::Conversations,
        Self::UniqueCustomers,
        Self::Organizations,
        Self::FirstResponses,
        Self::AgentReplies,
        Self::CustomerReplies,
        Self::Closures,
        Self::AvgFirstResponseMinutes,
        Self::AvgResolutionMinutes,
        Self::AvgWaitHours,
        Self::SlaBreached,
        Self::StateChanges,
        Self::AvgStateHours,
        Self::HighPriorityRate,
        Self::IssueLinkedShare,
        Self::AiAttributeShare,
        Self::AvgCustomerEffort,
        Self::HighFrictionRate,
        Self::CampaignSent,
        Self::CampaignReplies,
        Self::CampaignReplyRate,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversations => "conversations",
            Self::UniqueCustomers => "unique_customers",
            Self::Organizations => "organizations",
            Self::FirstResponses => "first_responses",
            Self::AgentReplies => "agent_replies",
            Self::CustomerReplies => "customer_replies",
            Self::Closures => "closures",
            Self::AvgFirstResponseMinutes => "avg_first_response_minutes",
            Self::AvgResolutionMinutes => "avg_resolution_minutes",
            Self::AvgWaitHours => "avg_wait_hours",
            Self::SlaBreached => "sla_breached",
            Self::StateChanges => "state_changes",
            Self::AvgStateHours => "avg_state_hours",
            Self::HighPriorityRate => "high_priority_rate",
            Self::IssueLinkedShare => "issue_linked_share",
            Self::AiAttributeShare => "ai_attribute_share",
            Self::AvgCustomerEffort => "avg_customer_effort",
            Self::HighFrictionRate => "high_friction_rate",
            Self::CampaignSent => "campaign_sent",
            Self::CampaignReplies => "campaign_replies",
            Self::CampaignReplyRate => "campaign_reply_rate",
        }
    }
}

/// One of the 14 report dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportDimensionKey {
    None,
    Day,
    Week,
    Month,
    Mailbox,
    Channel,
    Tag,
    Assignee,
    Team,
    Status,
    Priority,
    CustomState,
    ResponseState,
    Issue,
}

impl ReportDimensionKey {
    pub const ALL: [Self; 14] = [
        Self::None,
        Self::Day,
        Self::Week,
        Self::Month,
        Self::Mailbox,
        Self::Channel,
        Self::Tag,
        Self::Assignee,
        Self::Team,
        Self::Status,
        Self::Priority,
        Self::CustomState,
        Self::ResponseState,
        Self::Issue,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Mailbox => "mailbox",
            Self::Channel => "channel",
            Self::Tag => "tag",
            Self::Assignee => "assignee",
            Self::Team => "team",
            Self::Status => "status",
            Self::Priority => "priority",
            Self::CustomState => "custom_state",
            Self::ResponseState => "response_state",
            Self::Issue => "issue",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_count_matches_spec() {
        assert_eq!(ReportMetricKey::ALL.len(), 21);
    }

    #[test]
    fn dimension_count_matches_spec() {
        assert_eq!(ReportDimensionKey::ALL.len(), 14);
    }
}
