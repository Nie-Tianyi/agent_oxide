//! Trace event types for full-chain agent observability.
//!
//! Each event captures a single observable atom in the agent lifecycle.
//! Events are wrapped in [`Timestamped`] to carry both wall-clock offset
//! (via [`std::time::Instant`]) and absolute time (via
//! [`std::time::SystemTime`]).

use provider::Usage;
use std::time::{Duration, Instant, SystemTime};

// ── Timestamped wrapper ────────────────────────────────────────────────────────────

/// Wraps any event with capture-time metadata.
#[derive(Debug, Clone)]
pub struct Timestamped<T> {
    /// Monotonic wall-clock instant when the event was created.
    /// Use this to compute durations relative to run start.
    pub instant: Instant,
    /// Absolute system time when the event was created.
    /// Use this for ordering events across runs / processes.
    pub system_time: SystemTime,
    /// The wrapped event.
    pub inner: T,
}

impl<T> Timestamped<T> {
    pub fn new(inner: T) -> Self {
        Self {
            instant: Instant::now(),
            system_time: SystemTime::now(),
            inner,
        }
    }
}

// ── TraceEvent ────────────────────────────────────────────────────────────────────

/// A single observable event in the agent lifecycle.
///
/// Each variant carries the data available at that point in time.
/// Timing data is captured by the [`Timestamped`] wrapper.
#[derive(Debug, Clone)]
pub enum TraceEvent {
    /// A new agent run has started.
    RunStarted {
        session_id: String,
        /// Truncated to 200 chars for storage efficiency.
        user_input: String,
        max_steps: usize,
        max_retries: usize,
    },

    /// The agent run has finished (success, error, or cancelled).
    RunFinished {
        /// Human-readable outcome: "success", "error: …", "cancelled".
        outcome: String,
        /// Wall-clock duration of the entire run.
        total_duration: Duration,
        /// Number of ReAct loop iterations executed.
        total_steps: usize,
        /// Number of LLM API calls (including retries).
        total_llm_calls: usize,
        /// Number of tool executions (excluding rejections).
        total_tool_calls: usize,
        /// Cumulative token usage across all LLM calls.
        cumulative_usage: Usage,
    },

    /// A new ReAct loop iteration has started.
    StepStarted {
        /// 1-indexed step number.
        step: usize,
    },

    /// An LLM API call has started (HTTP request about to be sent).
    LlmCallStarted {
        /// Which ReAct step this call belongs to.
        step: usize,
        /// 0 = first attempt, 1+ = retry.
        attempt: usize,
        /// Number of messages in the context window.
        message_count: usize,
    },

    /// An LLM API call completed successfully.
    LlmCallFinished {
        step: usize,
        attempt: usize,
        /// Wall-clock duration of the LLM call (request sent → last chunk).
        duration: Duration,
        /// Token usage for this call.
        usage: Usage,
        /// Finish reason reported by the provider (e.g., "stop", "length", "tool_calls").
        finish_reason: Option<String>,
    },

    /// An LLM API call failed.
    LlmCallFailed {
        step: usize,
        attempt: usize,
        /// Human-readable error message.
        error: String,
        /// Whether the framework will retry this call.
        will_retry: bool,
        /// Duration until the failure was detected.
        duration: Duration,
    },

    /// A tool execution has started.
    ToolCallStarted {
        /// Unique tool call ID (from the LLM response).
        tool_call_id: String,
        /// Tool name (e.g., "read", "shell", "grep").
        tool_name: String,
        /// Which ReAct step triggered this tool.
        step: usize,
    },

    /// A tool execution has finished (success or failure).
    ToolCallFinished {
        tool_call_id: String,
        tool_name: String,
        /// Wall-clock duration of the tool execution.
        duration: Duration,
        /// `true` if the tool returned successfully.
        success: bool,
        /// Size of the tool output in bytes (0 on failure).
        output_size_bytes: usize,
    },

    /// A tool call was rejected by a hook (e.g., SandboxHook).
    ToolCallRejected {
        tool_call_id: String,
        tool_name: String,
        /// Reason for rejection (from the hook).
        reason: String,
    },

    /// Summary of streaming tokens emitted for a step.
    /// Emitted after the LLM stream completes.
    StreamingSummary {
        step: usize,
        /// Number of content (non-reasoning) token events emitted.
        content_chunks: usize,
        /// Number of reasoning token events emitted.
        reasoning_chunks: usize,
    },

    /// A subagent (child agent) has finished.
    SubagentFinished {
        /// The description/task name passed to the subagent.
        description: String,
        /// Number of ReAct steps the subagent executed.
        steps: usize,
        /// Number of LLM calls the subagent made.
        llm_calls: usize,
        /// Number of tool calls the subagent executed.
        tool_calls: usize,
        /// Token usage from the subagent (aggregated across all its LLM calls).
        usage: Usage,
        /// Wall-clock duration of the subagent run.
        duration: Duration,
    },
}
