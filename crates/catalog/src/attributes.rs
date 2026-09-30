//! AI attribute keys (14, verified against the reference).

use serde::{Deserialize, Serialize};

/// One of the 14 AI attribute keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiAttributeKey {
    Intent,
    Product,
    Feature,
    Issue,
    Urgency,
    FrustrationCues,
    TechnicalFamiliarity,
    CustomerGoal,
    QuestionCount,
    Risk,
    KnownIssue,
    IssueCluster,
    ResponseStyle,
    EscalationSignal,
}

impl AiAttributeKey {
    pub const ALL: [Self; 14] = [
        Self::Intent,
        Self::Product,
        Self::Feature,
        Self::Issue,
        Self::Urgency,
        Self::FrustrationCues,
        Self::TechnicalFamiliarity,
        Self::CustomerGoal,
        Self::QuestionCount,
        Self::Risk,
        Self::KnownIssue,
        Self::IssueCluster,
        Self::ResponseStyle,
        Self::EscalationSignal,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Product => "product",
            Self::Feature => "feature",
            Self::Issue => "issue",
            Self::Urgency => "urgency",
            Self::FrustrationCues => "frustration_cues",
            Self::TechnicalFamiliarity => "technical_familiarity",
            Self::CustomerGoal => "customer_goal",
            Self::QuestionCount => "question_count",
            Self::Risk => "risk",
            Self::KnownIssue => "known_issue",
            Self::IssueCluster => "issue_cluster",
            Self::ResponseStyle => "response_style",
            Self::EscalationSignal => "escalation_signal",
        }
    }

    /// The value type stored alongside the attribute.
    #[must_use]
    pub fn value_type(self) -> AttributeValueType {
        match self {
            Self::Intent
            | Self::Urgency
            | Self::FrustrationCues
            | Self::TechnicalFamiliarity
            | Self::Risk
            | Self::ResponseStyle => AttributeValueType::Enum,
            Self::Product
            | Self::Feature
            | Self::Issue
            | Self::CustomerGoal
            | Self::IssueCluster => AttributeValueType::Text,
            Self::QuestionCount => AttributeValueType::Number,
            Self::KnownIssue | Self::EscalationSignal => AttributeValueType::Boolean,
        }
    }
}

/// The value type an attribute carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeValueType {
    Enum,
    Text,
    Number,
    Boolean,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_spec() {
        assert_eq!(AiAttributeKey::ALL.len(), 14);
    }

    #[test]
    fn every_key_has_a_value_type() {
        for k in AiAttributeKey::ALL {
            // Just ensure the match is exhaustive.
            let _ = k.value_type();
        }
    }
}
