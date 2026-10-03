//! AI attribute keys (14, verified against the reference).
//!
//! Mirrors the reference's `AI_ATTRIBUTE_CATALOG` +
//! `AI_ATTRIBUTE_SCHEMA_VERSION` (`src/shared/constants.ts`): every key has a
//! label, a value type, a closed enum vocabulary where applicable, and a
//! description. The DB stores the key as TEXT; validation derives from this
//! single source of truth.

use serde::{Deserialize, Serialize};

/// Schema version stamped on every stored attribute row (versioning, plan
/// Phase 16). Mirrors `AI_ATTRIBUTE_SCHEMA_VERSION = 'attributes_v1'`.
pub const AI_ATTRIBUTE_SCHEMA_VERSION: &str = "attributes_v1";

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

    /// Parse a key from its storage string (closed catalog lookup).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|k| k.as_str() == s).copied()
    }

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

    /// The human label (reference `AI_ATTRIBUTE_CATALOG` `label`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Intent => "Intent",
            Self::Product => "Product",
            Self::Feature => "Feature",
            Self::Issue => "Issue type",
            Self::Urgency => "Urgency",
            Self::FrustrationCues => "Frustration cues",
            Self::TechnicalFamiliarity => "Technical familiarity",
            Self::CustomerGoal => "Customer goal",
            Self::QuestionCount => "Question count",
            Self::Risk => "Risk",
            Self::KnownIssue => "Known issue",
            Self::IssueCluster => "Issue cluster",
            Self::ResponseStyle => "Response style",
            Self::EscalationSignal => "Escalation signal",
        }
    }

    /// The catalog description (reference `AI_ATTRIBUTE_CATALOG`).
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Intent => {
                "What kind of request this is (question, bug report, \
                feature request, billing, how-to...)."
            }
            Self::Product => "Product the ticket is about, when identifiable from evidence.",
            Self::Feature => "Feature area the ticket touches, when identifiable from evidence.",
            Self::Issue => "Detected issue type (from AI ticket analysis when available).",
            Self::Urgency => {
                "Observable urgency cues in customer messages (deterministic by default)."
            }
            Self::FrustrationCues => {
                "Observable frustration cues in customer messages (deterministic by default)."
            }
            Self::TechnicalFamiliarity => {
                "Technical language level used by the customer (deterministic by default)."
            }
            Self::CustomerGoal => "One-line statement of what the customer is trying to achieve.",
            Self::QuestionCount => {
                "Number of questions the customer asked in this ticket (deterministic)."
            }
            Self::Risk => {
                "Composite churn/escalation risk from deterministic signals \
                (never a personality claim)."
            }
            Self::KnownIssue => "Whether the ticket is linked to a known issue.",
            Self::IssueCluster => "Issue cluster the ticket belongs to, when clustered.",
            Self::ResponseStyle => "Recommended response style for this interaction.",
            Self::EscalationSignal => "Whether the customer explicitly signals escalation intent.",
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

    /// The closed enum vocabulary for enum-typed keys (empty for others).
    /// Mirrors the reference's `AI_INTENT_VALUES` / `URGENCY_VALUES` /
    /// `FRUSTRATION_VALUES` / `TECHNICAL_VALUES` / `AI_RISK_VALUES` /
    /// `RESPONSE_PREFERENCE_VALUES`.
    #[must_use]
    pub fn values(self) -> &'static [&'static str] {
        match self {
            Self::Intent => &[
                "question",
                "bug_report",
                "feature_request",
                "billing",
                "how_to",
                "account_management",
                "feedback",
                "other",
            ],
            Self::Urgency => &["none", "low", "moderate", "high"],
            Self::FrustrationCues => &["none", "possible", "moderate", "strong"],
            Self::TechnicalFamiliarity => {
                &["non_technical", "mixed", "technical", "highly_technical"]
            }
            Self::Risk => &["low", "medium", "high"],
            Self::ResponseStyle => &[
                "concise",
                "detailed",
                "step_by_step",
                "technical",
                "conversational",
                "outcome_focused",
            ],
            _ => &[],
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

impl AttributeValueType {
    /// The storage string (reference `value_type` column).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enum => "enum",
            Self::Text => "text",
            Self::Number => "number",
            Self::Boolean => "boolean",
        }
    }

    /// Parse from the storage string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "enum" => Some(Self::Enum),
            "text" => Some(Self::Text),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            _ => None,
        }
    }
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

    #[test]
    fn parse_round_trips_every_key() {
        for k in AiAttributeKey::ALL {
            assert_eq!(AiAttributeKey::parse(k.as_str()), Some(k));
        }
        assert_eq!(AiAttributeKey::parse("not_a_key"), None);
    }

    #[test]
    fn enum_keys_carry_closed_vocabularies() {
        // Enum-typed keys have values; non-enum keys do not.
        for k in AiAttributeKey::ALL {
            match k.value_type() {
                AttributeValueType::Enum => assert!(!k.values().is_empty(), "{k:?}"),
                _ => assert!(k.values().is_empty(), "{k:?}"),
            }
        }
    }

    #[test]
    fn vocabularies_match_the_reference() {
        assert_eq!(
            AiAttributeKey::Intent.values(),
            &[
                "question",
                "bug_report",
                "feature_request",
                "billing",
                "how_to",
                "account_management",
                "feedback",
                "other"
            ]
        );
        assert_eq!(
            AiAttributeKey::Urgency.values(),
            &["none", "low", "moderate", "high"]
        );
        assert_eq!(
            AiAttributeKey::FrustrationCues.values(),
            &["none", "possible", "moderate", "strong"]
        );
        assert_eq!(AiAttributeKey::Risk.values(), &["low", "medium", "high"]);
        assert_eq!(
            AiAttributeKey::ResponseStyle.values(),
            &[
                "concise",
                "detailed",
                "step_by_step",
                "technical",
                "conversational",
                "outcome_focused"
            ]
        );
    }

    #[test]
    fn schema_version_is_attributes_v1() {
        assert_eq!(AI_ATTRIBUTE_SCHEMA_VERSION, "attributes_v1");
    }

    #[test]
    fn value_type_strings_round_trip() {
        for variant in [
            AttributeValueType::Enum,
            AttributeValueType::Text,
            AttributeValueType::Number,
            AttributeValueType::Boolean,
        ] {
            assert_eq!(AttributeValueType::parse(variant.as_str()), Some(variant));
        }
        assert_eq!(AttributeValueType::parse("nope"), None);
    }
}
