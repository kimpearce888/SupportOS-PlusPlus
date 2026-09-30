//! Copilot tools (22, verified against the reference).
//!
//! Per spec A10: "Also show the Copilot's read-only tool allowlist in the AI
//! Center for transparency." This enum is the single source of truth for the
//! 22 Copilot tools.
//!
//! Per spec: all Copilot tools are READ-ONLY — the Copilot never mutates
//! conversation state. Writes go through the write-protection pipeline
//! (`ticket_ops::execute` from M3-T05) via explicit human actions, never
//! automatically.
//!
//! Per the reference notes bounds:
//! - `COPILOT_MAX_TOOL_ROUNDS = 5`
//! - `COPILOT_MAX_TOOL_CALLS = 8`
//! - `COPILOT_TOOL_RESULT_MAX_CHARS = 4000`

use serde::{Deserialize, Serialize};

/// One of the 22 Copilot tools in the read-only allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopilotTool {
    /// Search conversations (FTS5 from M3-T06).
    SearchConversations,
    /// Search knowledge docs.
    SearchKnowledge,
    /// Search known issues (M7).
    SearchKnownIssues,
    /// Search saved replies.
    SearchSavedReplies,
    /// Get a single conversation by id.
    GetConversation,
    /// Get support metrics (dashboard).
    GetSupportMetrics,
    /// Get conversation context (customer + mailbox + tags).
    GetConversationContext,
    /// Get customer history (past conversations).
    GetCustomerHistory,
    /// Get similar conversations (vector search from M5-T01).
    GetSimilarConversations,
    /// Get issue clusters (M7).
    GetIssueClusters,
    /// Get AI analysis (M6-T02).
    GetAiAnalysis,
    /// Get SupportOS++ metadata (version, config).
    GetSupportosMetadata,
    /// Get AI attributes (M6-T03).
    GetAiAttributes,
    /// Search incidents (M7).
    SearchIncidents,
    /// Search custom objects (M10).
    SearchCustomObjects,
    /// Get customer timeline (M8).
    GetCustomerTimeline,
    /// Search connector data (M10).
    SearchConnectorData,
    /// Get knowledge gaps (M7).
    GetKnowledgeGaps,
    /// Get friction report (M8).
    GetFrictionReport,
    /// Get graph neighbors (M8 support graph).
    GetGraphNeighbors,
    /// Get graph stats (M8 support graph).
    GetGraphStats,
    /// Get customer memory (M6-T07).
    GetCustomerMemory,
}

impl CopilotTool {
    /// All 22 variants in spec order (matches the reference's `tools.ts`).
    pub const ALL: [Self; 22] = [
        Self::SearchConversations,
        Self::SearchKnowledge,
        Self::SearchKnownIssues,
        Self::SearchSavedReplies,
        Self::GetConversation,
        Self::GetSupportMetrics,
        Self::GetConversationContext,
        Self::GetCustomerHistory,
        Self::GetSimilarConversations,
        Self::GetIssueClusters,
        Self::GetAiAnalysis,
        Self::GetSupportosMetadata,
        Self::GetAiAttributes,
        Self::SearchIncidents,
        Self::SearchCustomObjects,
        Self::GetCustomerTimeline,
        Self::SearchConnectorData,
        Self::GetKnowledgeGaps,
        Self::GetFrictionReport,
        Self::GetGraphNeighbors,
        Self::GetGraphStats,
        Self::GetCustomerMemory,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SearchConversations => "search_conversations",
            Self::SearchKnowledge => "search_knowledge",
            Self::SearchKnownIssues => "search_known_issues",
            Self::SearchSavedReplies => "search_saved_replies",
            Self::GetConversation => "get_conversation",
            Self::GetSupportMetrics => "get_support_metrics",
            Self::GetConversationContext => "get_conversation_context",
            Self::GetCustomerHistory => "get_customer_history",
            Self::GetSimilarConversations => "get_similar_conversations",
            Self::GetIssueClusters => "get_issue_clusters",
            Self::GetAiAnalysis => "get_ai_analysis",
            Self::GetSupportosMetadata => "get_supportos_metadata",
            Self::GetAiAttributes => "get_ai_attributes",
            Self::SearchIncidents => "search_incidents",
            Self::SearchCustomObjects => "search_custom_objects",
            Self::GetCustomerTimeline => "get_customer_timeline",
            Self::SearchConnectorData => "search_connector_data",
            Self::GetKnowledgeGaps => "get_knowledge_gaps",
            Self::GetFrictionReport => "get_friction_report",
            Self::GetGraphNeighbors => "get_graph_neighbors",
            Self::GetGraphStats => "get_graph_stats",
            Self::GetCustomerMemory => "get_customer_memory",
        }
    }

    /// A human-readable description of what the tool does.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::SearchConversations => "Search conversations by text (FTS5).",
            Self::SearchKnowledge => "Search knowledge base documents.",
            Self::SearchKnownIssues => "Search known issues by name or status.",
            Self::SearchSavedReplies => "Search saved reply templates.",
            Self::GetConversation => "Get a single conversation by id.",
            Self::GetSupportMetrics => "Get current support metrics (response times, volumes).",
            Self::GetConversationContext => "Get conversation context (customer, mailbox, tags).",
            Self::GetCustomerHistory => "Get a customer's past conversations.",
            Self::GetSimilarConversations => "Find similar conversations via vector search.",
            Self::GetIssueClusters => "Get clusters of similar issues.",
            Self::GetAiAnalysis => "Get AI analysis for a conversation.",
            Self::GetSupportosMetadata => "Get SupportOS++ version and configuration metadata.",
            Self::GetAiAttributes => "Get AI-derived attributes for a conversation.",
            Self::SearchIncidents => "Search incidents by status or severity.",
            Self::SearchCustomObjects => "Search custom object types.",
            Self::GetCustomerTimeline => "Get a customer's activity timeline.",
            Self::SearchConnectorData => "Search data from configured connectors.",
            Self::GetKnowledgeGaps => "Get identified knowledge gaps.",
            Self::GetFrictionReport => "Get friction metrics for support interactions.",
            Self::GetGraphNeighbors => "Get neighbors of a node in the support graph.",
            Self::GetGraphStats => "Get statistics about the support graph.",
            Self::GetCustomerMemory => "Get AI-derived memory for a customer.",
        }
    }
}

/// The maximum number of tool rounds (back-and-forth between the AI and tools).
/// Per the reference notes: `COPILOT_MAX_TOOL_ROUNDS = 5`.
pub const MAX_TOOL_ROUNDS: u32 = 5;

/// The maximum number of tool calls per Copilot run.
/// Per the reference notes: `COPILOT_MAX_TOOL_CALLS = 8`.
pub const MAX_TOOL_CALLS: u32 = 8;

/// The maximum character length of a single tool result.
/// Per the reference notes: `COPILOT_TOOL_RESULT_MAX_CHARS = 4000`.
pub const TOOL_RESULT_MAX_CHARS: usize = 4000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_spec() {
        assert_eq!(CopilotTool::ALL.len(), 22);
    }

    #[test]
    fn as_str_matches_inventory() {
        let expected = [
            "search_conversations",
            "search_knowledge",
            "search_known_issues",
            "search_saved_replies",
            "get_conversation",
            "get_support_metrics",
            "get_conversation_context",
            "get_customer_history",
            "get_similar_conversations",
            "get_issue_clusters",
            "get_ai_analysis",
            "get_supportos_metadata",
            "get_ai_attributes",
            "search_incidents",
            "search_custom_objects",
            "get_customer_timeline",
            "search_connector_data",
            "get_knowledge_gaps",
            "get_friction_report",
            "get_graph_neighbors",
            "get_graph_stats",
            "get_customer_memory",
        ];
        for (tool, expected_str) in CopilotTool::ALL.iter().zip(expected.iter()) {
            assert_eq!(tool.as_str(), *expected_str);
        }
    }

    #[test]
    fn all_tools_have_descriptions() {
        for tool in CopilotTool::ALL {
            assert!(
                !tool.description().is_empty(),
                "{tool:?} missing description"
            );
        }
    }

    #[test]
    fn serde_round_trip() {
        for tool in CopilotTool::ALL {
            let s = serde_json::to_string(&tool).unwrap();
            let back: CopilotTool = serde_json::from_str(&s).unwrap();
            assert_eq!(tool, back);
        }
    }

    #[test]
    fn bounds_match_reference() {
        assert_eq!(MAX_TOOL_ROUNDS, 5);
        assert_eq!(MAX_TOOL_CALLS, 8);
        assert_eq!(TOOL_RESULT_MAX_CHARS, 4000);
    }
}
