//! Operations Center tile keys (16, verified against the reference).

use serde::{Deserialize, Serialize};

/// One of the 16 Operations Center tile keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationsTileKey {
    Unassigned,
    NeedsFirstResponse,
    CustomerWaiting,
    WaitingOverThreshold,
    Urgent,
    SlaAtRisk,
    SlaBreached,
    HighEffort,
    RepeatedIssue,
    KnownIssue,
    AiEscalation,
    IssueSpike,
    AutomationApprovals,
    FailedJobs,
    SyncProblems,
    CampaignActivity,
}

impl OperationsTileKey {
    /// All 16 variants in spec order.
    pub const ALL: [Self; 16] = [
        Self::Unassigned,
        Self::NeedsFirstResponse,
        Self::CustomerWaiting,
        Self::WaitingOverThreshold,
        Self::Urgent,
        Self::SlaAtRisk,
        Self::SlaBreached,
        Self::HighEffort,
        Self::RepeatedIssue,
        Self::KnownIssue,
        Self::AiEscalation,
        Self::IssueSpike,
        Self::AutomationApprovals,
        Self::FailedJobs,
        Self::SyncProblems,
        Self::CampaignActivity,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unassigned => "unassigned",
            Self::NeedsFirstResponse => "needs_first_response",
            Self::CustomerWaiting => "customer_waiting",
            Self::WaitingOverThreshold => "waiting_over_threshold",
            Self::Urgent => "urgent",
            Self::SlaAtRisk => "sla_at_risk",
            Self::SlaBreached => "sla_breached",
            Self::HighEffort => "high_effort",
            Self::RepeatedIssue => "repeated_issue",
            Self::KnownIssue => "known_issue",
            Self::AiEscalation => "ai_escalation",
            Self::IssueSpike => "issue_spike",
            Self::AutomationApprovals => "automation_approvals",
            Self::FailedJobs => "failed_jobs",
            Self::SyncProblems => "sync_problems",
            Self::CampaignActivity => "campaign_activity",
        }
    }

    /// Severity bucket the tile reports under (reference
    /// operationsCenter.ts:48-183 — `sync_problems` overrides dynamically:
    /// critical when the sync state is ERROR, else info).
    #[must_use]
    pub fn severity(self) -> &'static str {
        match self {
            Self::Unassigned
            | Self::CustomerWaiting
            | Self::HighEffort
            | Self::RepeatedIssue
            | Self::KnownIssue
            | Self::CampaignActivity => "info",
            Self::NeedsFirstResponse
            | Self::WaitingOverThreshold
            | Self::Urgent
            | Self::SlaAtRisk
            | Self::AiEscalation
            | Self::IssueSpike
            | Self::AutomationApprovals
            | Self::FailedJobs
            | Self::SyncProblems => "warning",
            Self::SlaBreached => "critical",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_spec() {
        assert_eq!(OperationsTileKey::ALL.len(), 16);
    }

    #[test]
    fn severity_buckets_exist() {
        for k in OperationsTileKey::ALL {
            let s = k.severity();
            assert!(matches!(s, "info" | "warning" | "critical"));
        }
    }

    #[test]
    fn serde_round_trip() {
        for k in OperationsTileKey::ALL {
            let s = serde_json::to_string(&k).unwrap();
            let back: OperationsTileKey = serde_json::from_str(&s).unwrap();
            assert_eq!(k, back);
        }
    }
}
