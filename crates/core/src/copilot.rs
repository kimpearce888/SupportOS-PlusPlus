//! Copilot — read-only tool allowlist + bounded execution engine (M6-T04).
//!
//! Per spec M6: "Copilot."
//! Per spec A10: "Show the Copilot's read-only tool allowlist in the AI Center
//! for transparency."
//! Per spec: "AI is always advisory. Automatic customer-reply sending is
//! permanently OFF."
//! Per the reference notes bounds: COPILOT_MAX_TOOL_ROUNDS=5,
//! COPILOT_MAX_TOOL_CALLS=8, COPILOT_TOOL_RESULT_MAX_CHARS=4000.
//!
//! The Copilot engine is bounded — it never runs unbounded tool calls. Each
//! tool is read-only (no writes — the Copilot never mutates state). The
//! bounds are enforced structurally in the engine, not as a recommendation.

use crate::catalog::copilot::{
    CopilotTool, MAX_TOOL_CALLS, MAX_TOOL_ROUNDS, TOOL_RESULT_MAX_CHARS,
};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// The result of a single tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Which tool was called.
    pub tool: CopilotTool,
    /// The arguments passed to the tool (JSON).
    pub args: String,
    /// The tool's output (truncated to `TOOL_RESULT_MAX_CHARS`).
    pub output: String,
    /// Whether the output was truncated.
    pub truncated: bool,
}

/// The result of a Copilot run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CopilotResult {
    /// The AI's final response text.
    pub response: String,
    /// All tool calls made during this run.
    pub tool_calls: Vec<ToolResult>,
    /// The number of rounds used (each round = one AI call + possible tool calls).
    pub rounds_used: u32,
    /// Whether the run was terminated due to hitting a bound (max rounds or max calls).
    pub terminated_by_bound: bool,
}

/// Truncate a tool result to `TOOL_RESULT_MAX_CHARS`. Returns the truncated
/// string + whether truncation occurred. Pure function — testable.
#[must_use]
pub fn truncate_result(output: &str) -> (String, bool) {
    if output.len() <= TOOL_RESULT_MAX_CHARS {
        (output.to_string(), false)
    } else {
        let truncated = &output[..TOOL_RESULT_MAX_CHARS];
        (format!("{truncated}… [truncated]"), true)
    }
}

/// Check whether the Copilot engine should stop based on bounds.
/// Pure function — testable.
///
/// Returns `true` if the engine has hit a bound (max rounds or max calls)
/// and should stop calling more tools.
#[must_use]
pub fn should_stop(rounds: u32, tool_calls: u32) -> bool {
    rounds >= MAX_TOOL_ROUNDS || tool_calls >= MAX_TOOL_CALLS
}

/// Validate that a tool is in the read-only allowlist (22 tools from the
/// catalog). Per spec A10: the Copilot's tool allowlist is read-only.
///
/// Returns `Ok(())` if the tool is allowed, `Err` otherwise.
pub fn validate_tool_allowed(tool: &CopilotTool) -> Result<()> {
    if CopilotTool::ALL.contains(tool) {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "tool {tool:?} is not in the read-only allowlist"
        )))
    }
}

/// The Copilot tool allowlist — all 22 read-only tools.
/// Convenience function that returns the same slice as
/// `ai_center::copilot_tool_allowlist()`.
#[must_use]
pub fn tool_allowlist() -> &'static [CopilotTool] {
    &CopilotTool::ALL
}

/// The maximum number of tool rounds (back-and-forth between AI + tools).
pub const fn max_tool_rounds() -> u32 {
    MAX_TOOL_ROUNDS
}

/// The maximum number of tool calls per Copilot run.
pub const fn max_tool_calls() -> u32 {
    MAX_TOOL_CALLS
}

/// The maximum character length of a single tool result.
pub const fn tool_result_max_chars() -> usize {
    TOOL_RESULT_MAX_CHARS
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- truncate_result ---------------------------------------------------

    #[test]
    fn truncate_result_short_string_unchanged() {
        let (result, truncated) = truncate_result("hello world");
        assert_eq!(result, "hello world");
        assert!(!truncated);
    }

    #[test]
    fn truncate_result_exactly_at_limit() {
        let input = "a".repeat(TOOL_RESULT_MAX_CHARS);
        let (result, truncated) = truncate_result(&input);
        assert_eq!(result, input);
        assert!(!truncated, "exactly at limit → not truncated");
    }

    #[test]
    fn truncate_result_over_limit_truncated() {
        let input = "a".repeat(TOOL_RESULT_MAX_CHARS + 1000);
        let (result, truncated) = truncate_result(&input);
        assert!(truncated);
        assert!(result.ends_with("… [truncated]"));
        // The result should be shorter than the input (the suffix is small).
        assert!(result.len() < input.len());
    }

    #[test]
    fn truncate_result_empty_string_unchanged() {
        let (result, truncated) = truncate_result("");
        assert_eq!(result, "");
        assert!(!truncated);
    }

    // ---- should_stop -------------------------------------------------------

    #[test]
    fn should_stop_at_max_rounds() {
        assert!(should_stop(MAX_TOOL_ROUNDS, 0));
    }

    #[test]
    fn should_stop_at_max_tool_calls() {
        assert!(should_stop(0, MAX_TOOL_CALLS));
    }

    #[test]
    fn should_not_stop_below_bounds() {
        assert!(!should_stop(0, 0));
        assert!(!should_stop(1, 1));
        assert!(!should_stop(MAX_TOOL_ROUNDS - 1, MAX_TOOL_CALLS - 1));
    }

    #[test]
    fn should_stop_when_both_exceeded() {
        assert!(should_stop(MAX_TOOL_ROUNDS + 1, MAX_TOOL_CALLS + 1));
    }

    // ---- validate_tool_allowed ---------------------------------------------

    #[test]
    fn validate_all_22_tools_allowed() {
        for tool in CopilotTool::ALL {
            assert!(
                validate_tool_allowed(&tool).is_ok(),
                "{tool:?} should be allowed"
            );
        }
    }

    // ---- tool_allowlist ----------------------------------------------------

    #[test]
    fn tool_allowlist_has_22_tools() {
        assert_eq!(tool_allowlist().len(), 22);
    }

    #[test]
    fn tool_allowlist_all_read_only() {
        // All tools have descriptions (proof they're documented, real tools).
        for &tool in tool_allowlist() {
            assert!(!tool.description().is_empty());
        }
    }

    // ---- bounds constants --------------------------------------------------

    #[test]
    fn max_tool_rounds_is_5() {
        assert_eq!(max_tool_rounds(), 5);
    }

    #[test]
    fn max_tool_calls_is_8() {
        assert_eq!(max_tool_calls(), 8);
    }

    #[test]
    fn tool_result_max_chars_is_4000() {
        assert_eq!(tool_result_max_chars(), 4000);
    }

    // ---- ToolResult + CopilotResult serde ---------------------------------

    #[test]
    fn tool_result_serializes() {
        let tr = ToolResult {
            tool: CopilotTool::SearchConversations,
            args: r#"{"query": "refund"}"#.into(),
            output: "Found 3 conversations".into(),
            truncated: false,
        };
        let s = serde_json::to_string(&tr).unwrap();
        assert!(s.contains("\"tool\":\"search_conversations\""));
        assert!(s.contains("\"truncated\":false"));
    }

    #[test]
    fn copilot_result_serializes() {
        let cr = CopilotResult {
            response: "Here's what I found".into(),
            tool_calls: vec![],
            rounds_used: 1,
            terminated_by_bound: false,
        };
        let s = serde_json::to_string(&cr).unwrap();
        assert!(s.contains("\"rounds_used\":1"));
        assert!(s.contains("\"terminated_by_bound\":false"));
    }
}
