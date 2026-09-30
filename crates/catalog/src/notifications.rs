//! Notification types (15, verified against the reference).

use serde::{Deserialize, Serialize};

/// One of the 15 notification types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationType {
    CustomerReplied,
    TicketAssigned,
    Mentioned,
    TeamMentioned,
    SlaRisk,
    SlaBreach,
    AutomationApproval,
    AiEscalation,
    KnownIssueDetected,
    IssueSpike,
    CampaignReply,
    SyncFailure,
    JobFailure,
    CustomerEvent,
    IncidentUpdate,
}

impl NotificationType {
    /// All 15 variants in spec order.
    pub const ALL: [Self; 15] = [
        Self::CustomerReplied,
        Self::TicketAssigned,
        Self::Mentioned,
        Self::TeamMentioned,
        Self::SlaRisk,
        Self::SlaBreach,
        Self::AutomationApproval,
        Self::AiEscalation,
        Self::KnownIssueDetected,
        Self::IssueSpike,
        Self::CampaignReply,
        Self::SyncFailure,
        Self::JobFailure,
        Self::CustomerEvent,
        Self::IncidentUpdate,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CustomerReplied => "customer_replied",
            Self::TicketAssigned => "ticket_assigned",
            Self::Mentioned => "mentioned",
            Self::TeamMentioned => "team_mentioned",
            Self::SlaRisk => "sla_risk",
            Self::SlaBreach => "sla_breach",
            Self::AutomationApproval => "automation_approval",
            Self::AiEscalation => "ai_escalation",
            Self::KnownIssueDetected => "known_issue_detected",
            Self::IssueSpike => "issue_spike",
            Self::CampaignReply => "campaign_reply",
            Self::SyncFailure => "sync_failure",
            Self::JobFailure => "job_failure",
            Self::CustomerEvent => "customer_event",
            Self::IncidentUpdate => "incident_update",
        }
    }

    /// Whether the type is enabled by default for new users.
    #[must_use]
    pub fn default_enabled(self) -> bool {
        !matches!(self, Self::CampaignReply | Self::CustomerEvent)
    }

    /// Severity bucket for sorting in the UI.
    #[must_use]
    pub fn severity(self) -> &'static str {
        match self {
            Self::SlaBreach | Self::SyncFailure | Self::JobFailure => "critical",
            Self::SlaRisk
            | Self::AiEscalation
            | Self::IssueSpike
            | Self::KnownIssueDetected
            | Self::IncidentUpdate => "warning",
            _ => "info",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_spec() {
        assert_eq!(NotificationType::ALL.len(), 15);
    }

    #[test]
    fn default_enabled_is_sane() {
        // High-signal types are on by default; campaign_reply stays opt-in.
        assert!(NotificationType::SlaBreach.default_enabled());
        assert!(!NotificationType::CampaignReply.default_enabled());
    }
}
