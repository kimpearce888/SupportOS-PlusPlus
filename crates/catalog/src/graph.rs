//! Graph node kinds (12, verified against the reference).

use serde::{Deserialize, Serialize};

/// One of the 12 graph node kinds in the support graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    Customer,
    Organization,
    Conversation,
    KnownIssue,
    IssueCluster,
    Incident,
    KnowledgeDocument,
    Agent,
    Campaign,
    Product,
    CustomObject,
    ConnectorData,
}

impl GraphNodeKind {
    pub const ALL: [Self; 12] = [
        Self::Customer,
        Self::Organization,
        Self::Conversation,
        Self::KnownIssue,
        Self::IssueCluster,
        Self::Incident,
        Self::KnowledgeDocument,
        Self::Agent,
        Self::Campaign,
        Self::Product,
        Self::CustomObject,
        Self::ConnectorData,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Customer => "customer",
            Self::Organization => "organization",
            Self::Conversation => "conversation",
            Self::KnownIssue => "known_issue",
            Self::IssueCluster => "issue_cluster",
            Self::Incident => "incident",
            Self::KnowledgeDocument => "knowledge_document",
            Self::Agent => "agent",
            Self::Campaign => "campaign",
            Self::Product => "product",
            Self::CustomObject => "custom_object",
            Self::ConnectorData => "connector_data",
        }
    }

    /// Parse the wire vocabulary (the 12-kind union the routes accept).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_spec() {
        assert_eq!(GraphNodeKind::ALL.len(), 12);
    }
}

/// One of the 5 relations a HUMAN may assert explicitly on the support
/// graph (the closed union; reference shared/graph.ts
/// GRAPH_HUMAN_RELATIONS, plan Phase 34). Everything else the graph
/// serves is derived at read time and never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphHumanRelation {
    RelatedTo,
    DependsOn,
    Blocks,
    Mentions,
    DuplicateOf,
}

impl GraphHumanRelation {
    pub const ALL: [Self; 5] = [
        Self::RelatedTo,
        Self::DependsOn,
        Self::Blocks,
        Self::Mentions,
        Self::DuplicateOf,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RelatedTo => "related_to",
            Self::DependsOn => "depends_on",
            Self::Blocks => "blocks",
            Self::Mentions => "mentions",
            Self::DuplicateOf => "duplicate_of",
        }
    }

    /// Parse the wire vocabulary (the value set the routes accept and the
    /// CHECK constraint on graph_edges enforces).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| r.as_str() == s)
    }
}

#[cfg(test)]
mod human_relation_tests {
    use super::*;

    #[test]
    fn five_relations_in_reference_order() {
        let wire: Vec<&str> = GraphHumanRelation::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            wire,
            vec![
                "related_to",
                "depends_on",
                "blocks",
                "mentions",
                "duplicate_of"
            ]
        );
    }

    #[test]
    fn parse_round_trip() {
        for r in GraphHumanRelation::ALL {
            assert_eq!(GraphHumanRelation::parse(r.as_str()), Some(r));
        }
        assert_eq!(GraphHumanRelation::parse("related"), None);
        assert_eq!(GraphHumanRelation::parse("replied_to"), None);
    }
}
