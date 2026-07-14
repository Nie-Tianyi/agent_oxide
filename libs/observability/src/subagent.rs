//! Subagent trace aggregation.

use provider::Usage;
use std::time::Duration;

/// Aggregated trace summary from a child agent run.
///
/// Written by the subagent's [`ObservabilityHook`](super::hooks::ObservabilityHook)
/// on finish and read by the parent to emit a single
/// [`TraceEvent::SubagentFinished`](super::event::TraceEvent::SubagentFinished).
#[derive(Debug, Clone)]
pub struct SubagentTrace {
    /// The description/task name passed to the subagent.
    pub description: String,
    /// Number of ReAct loop iterations the subagent executed.
    pub steps: usize,
    /// Number of LLM API calls the subagent made.
    pub llm_calls: usize,
    /// Number of tool calls the subagent executed.
    pub tool_calls: usize,
    /// Aggregated token usage across all LLM calls.
    pub usage: Usage,
    /// Wall-clock duration of the subagent run.
    pub duration: Duration,
}

impl Default for SubagentTrace {
    fn default() -> Self {
        Self {
            description: String::new(),
            steps: 0,
            llm_calls: 0,
            tool_calls: 0,
            usage: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
            duration: Duration::ZERO,
        }
    }
}
