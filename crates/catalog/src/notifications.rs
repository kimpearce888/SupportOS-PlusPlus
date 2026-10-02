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
    /// The reference enables ALL 15 types by default
    /// (NOTIFICATION_TYPE_DEFAULT_ENABLED in src/shared/collaboration.ts).
    pub fn default_enabled(self) -> bool {
        true
    }

    /// Severity bucket for sorting in the UI.
    #[must_use]
    /// Severity per type — the exact reference
    /// NOTIFICATION_SEVERITY_BY_TYPE map.
    pub fn severity(self) -> &'static str {
        match self {
            Self::SlaBreach | Self::SyncFailure => "critical",
            Self::SlaRisk
            | Self::AutomationApproval
            | Self::AiEscalation
            | Self::IssueSpike
            | Self::JobFailure
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
    fn default_enabled_matches_reference() {
        // The reference enables ALL types by default.
        for t in NotificationType::ALL {
            assert!(t.default_enabled(), "{t:?} must be default-enabled");
        }
    }

    #[test]
    fn severity_matches_reference_map() {
        let expected = [
            (NotificationType::CustomerReplied, "info"),
            (NotificationType::TicketAssigned, "info"),
            (NotificationType::Mentioned, "info"),
            (NotificationType::TeamMentioned, "info"),
            (NotificationType::SlaRisk, "warning"),
            (NotificationType::SlaBreach, "critical"),
            (NotificationType::AutomationApproval, "warning"),
            (NotificationType::AiEscalation, "warning"),
            (NotificationType::KnownIssueDetected, "info"),
            (NotificationType::IssueSpike, "warning"),
            (NotificationType::CampaignReply, "info"),
            (NotificationType::SyncFailure, "critical"),
            (NotificationType::JobFailure, "warning"),
            (NotificationType::CustomerEvent, "info"),
            (NotificationType::IncidentUpdate, "warning"),
        ];
        for (t, sev) in expected {
            assert_eq!(t.severity(), sev, "{t:?} severity mismatch");
        }
    }
}
