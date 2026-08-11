use memory::SharedMemory;
use provider::{Message, ProviderError, ToolCall};

use crate::agent::{AgentError, RunOutcome};

/// Lifecycle hook for observing and intervening in agent execution.
///
/// All methods have default no-op implementations — implement only
/// the events you care about. All methods are synchronous.
///
/// For async work (e.g. LLM summarisation / macro-compaction), use
/// [`crate::block_on`] inside a sync hook method.  Do **not** call
/// `Handle::block_on` directly — hooks run on a tokio worker thread,
/// where a bare `block_on` panics with "Cannot block_on from within
/// a runtime".  The agent loop runs in a dedicated tokio task separate
/// from the TUI thread, so blocking here does not affect the UI.
/// See [`MacroCompactHook`](hooks::MacroCompactHook) for an example.
///
/// ## Naming convention
///
/// | Prefix | Meaning |
/// |--------|---------|
/// | `on_<event>` | Pure notification — cannot intervene |
/// | `before_<action>` | Can intervene — return `Err` to block |
/// | `after_<action>` | Observe the result — cannot intervene |
///
/// ## Extension points
///
/// | Method | Called | Phase |
/// |--------|--------|-------|
/// | [`on_run_start`](Self::on_run_start) | When a new task begins | Run |
/// | [`on_run_finish`](Self::on_run_finish) | When the task terminates | Run |
/// | [`on_step_start`](Self::on_step_start) | Top of each ReAct loop iteration | Step |
/// | [`on_llm_start`](Self::on_llm_start) | Before building context for LLM | LLM |
/// | [`on_llm_end`](Self::on_llm_end) | After LLM response (success) | LLM |
/// | [`on_llm_error`](Self::on_llm_error) | When an LLM call fails | LLM |
/// | [`before_tool_call`](Self::before_tool_call) | Before tool execution | Tool |
/// | [`after_tool_call`](Self::after_tool_call) | After tool execution (success) | Tool |
/// | [`on_tool_failed`](Self::on_tool_failed) | When tool execution fails | Tool |
pub trait AgentHook: Send + Sync {
    // ── Run lifecycle ─────────────────────────────────────────────────────────

    /// Called when a new user input begins a full task run.
    fn on_run_start(&self, _session_id: &str, _user_input: &str, _memory: &SharedMemory) {}

    /// Called when the task terminates — success, error, or cancellation.
    ///
    /// The [`RunOutcome`] discriminates the three cases.  This is the single
    /// place to hook run-level teardown (audit trail closure, resource summary,
    /// persistence triggers, cleanup).
    fn on_run_finish(&self, _session_id: &str, _outcome: &RunOutcome, _memory: &SharedMemory) {}

    // ── Step lifecycle ───────────────────────────────────────────────────────

    /// Called at the top of each ReAct loop iteration, before
    /// [`on_llm_start`](Self::on_llm_start) and before the `max_steps` check.
    ///
    /// `step` is 1-indexed; `max_steps` is the configured limit.  Use this to
    /// track progress or emit early warnings when approaching the limit.
    fn on_step_start(&self, _session_id: &str, _step: usize, _max_steps: usize) {}

    // ── LLM lifecycle ────────────────────────────────────────────────────────

    /// Called before building the context vector for each LLM call.
    ///
    /// Receives shared memory so the hook can compact or transform
    /// messages in-place (e.g. tool-output clearing).
    fn on_llm_start(&self, _session_id: &str, _memory: &SharedMemory) {}

    /// Called after receiving a response from the LLM.
    fn on_llm_end(&self, _session_id: &str, _response: &Message) {}

    /// Called when an LLM provider call fails.
    ///
    /// `attempt` is 0-indexed (0 = first failure).  `will_retry` is `true`
    /// when the framework will retry after exponential backoff, `false`
    /// when this is the terminal failure (retries exhausted or non-retryable
    /// error).
    fn on_llm_error(
        &self,
        _session_id: &str,
        _error: &ProviderError,
        _attempt: usize,
        _will_retry: bool,
    ) {
    }

    // ── Tool lifecycle ───────────────────────────────────────────────────────

    /// Called before executing a tool.
    ///
    /// Return `Err(AgentError::ToolRejected)` to skip the tool and add
    /// the error message as the observation instead.
    fn before_tool_call(&self, _session_id: &str, _tool_call: &ToolCall) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called after a tool has been executed successfully, with its
    /// observation output.
    fn after_tool_call(&self, _session_id: &str, _tool_call: &ToolCall, _observation: &str) {}

    /// Called when a tool execution fails — the tool returned an error
    /// or was not found in the registry.
    ///
    /// Pairs with [`after_tool_call`](Self::after_tool_call) which fires
    /// only on success.  Use this to distinguish failures in audit logs
    /// and track per-tool error rates.
    fn on_tool_failed(&self, _session_id: &str, _tool_call: &ToolCall, _error: &str) {}
}
