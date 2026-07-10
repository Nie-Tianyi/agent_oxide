use memory::SharedMemory;
use provider::{Message, ToolCall};

use crate::agent::AgentError;

/// Lifecycle hook for observing and intervening in agent execution.
///
/// All methods have default no-op implementations — implement only
/// the events you care about. All methods are synchronous.
///
/// For async work (e.g. LLM summarisation / macro-compaction), use
/// a dedicated component — the agent loop provides a separate
/// `before_llm_async` hook point for that purpose.
///
/// ## Extension points
///
/// | Method | Called | Can intervene |
/// |--------|--------|----------------|
/// | [`on_run_start`](Self::on_run_start) | When a new task begins | No |
/// | [`on_llm_start`](Self::on_llm_start) | Before building context for LLM | Yes — mutate memory (tool-output clearing) |
/// | [`on_llm_end`](Self::on_llm_end) | After LLM response | No |
/// | [`before_tool_call`](Self::before_tool_call) | Before tool execution | **Yes — return Err to block** |
/// | [`after_tool_call`](Self::after_tool_call) | After tool execution | No |
#[allow(unused_variables)]
pub trait AgentHook: Send + Sync {
    /// Called when a new user input begins a full task run.
    fn on_run_start(&self, session_id: &str, user_input: &str) {}

    /// Called before building the context vector for each LLM call.
    ///
    /// Receives shared memory so the hook can compact or transform
    /// messages in-place (e.g. tool-output clearing).
    fn on_llm_start(&self, session_id: &str, memory: &SharedMemory) {}

    /// Called after receiving a response from the LLM.
    fn on_llm_end(&self, session_id: &str, response: &Message) {}

    /// Called before executing a tool.
    ///
    /// Return `Err(AgentError::ToolRejected)` to skip the tool and add
    /// the error message as the observation instead.
    fn before_tool_call(&self, session_id: &str, tool_call: &ToolCall) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called after a tool has been executed, with its observation.
    fn after_tool_call(&self, session_id: &str, tool_call: &ToolCall, observation: &str) {}
}
