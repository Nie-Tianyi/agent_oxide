//! Trace event types for agent observability.
//!
//! Each variant captures a single observable atom in the agent lifecycle.
//! Events are dispatched to the [`tracing`] crate via [`super::store::TraceStore::emit`]
//! and can be persisted to the file log or filtered by target (`agent`) and level.

use provider::Usage;
use std::fmt;
use std::time::Duration;

// ── TraceEvent ────────────────────────────────────────────────────────────────────

/// A single observable event in the agent lifecycle.
///
/// Each variant carries the data available at that point in time.
/// Timing data is captured by the caller (e.g. [`ObservabilityHook`])
/// before the event is emitted.
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
    ///
    /// **Reserved for future use** — currently never emitted by any code path.
    ToolCallRejected {
        tool_call_id: String,
        tool_name: String,
        /// Reason for rejection (from the hook).
        reason: String,
    },

    /// Summary of streaming tokens emitted for a step.
    ///
    /// **Reserved for future use** — currently never emitted by any code path.
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

// ── Display ───────────────────────────────────────────────────────────────────────

impl fmt::Display for TraceEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunStarted {
                session_id,
                user_input,
                max_steps,
                max_retries,
            } => {
                write!(
                    f,
                    "RunStarted session={session_id} input=\"{user_input}\" max_steps={max_steps} max_retries={max_retries}"
                )
            }
            Self::RunFinished {
                outcome,
                total_duration,
                total_steps,
                total_llm_calls,
                total_tool_calls,
                cumulative_usage,
            } => {
                write!(
                    f,
                    "RunFinished outcome=\"{outcome}\" duration={}ms steps={total_steps} llm={total_llm_calls} tools={total_tool_calls} tokens={}",
                    total_duration.as_millis(),
                    cumulative_usage.total_tokens
                )
            }
            Self::StepStarted { step } => {
                write!(f, "StepStarted step={step}")
            }
            Self::LlmCallStarted {
                step,
                attempt,
                message_count,
            } => {
                write!(
                    f,
                    "LlmCallStarted step={step} attempt={attempt} msgs={message_count}"
                )
            }
            Self::LlmCallFinished {
                step,
                attempt,
                duration,
                usage,
                finish_reason,
            } => {
                write!(
                    f,
                    "LlmCallFinished step={step} attempt={attempt} duration={}ms tokens={} reason={}",
                    duration.as_millis(),
                    usage.total_tokens,
                    finish_reason.as_deref().unwrap_or("none")
                )
            }
            Self::LlmCallFailed {
                step,
                attempt,
                error,
                will_retry,
                duration,
            } => {
                write!(
                    f,
                    "LlmCallFailed step={step} attempt={attempt} error=\"{error}\" retry={will_retry} duration={}ms",
                    duration.as_millis()
                )
            }
            Self::ToolCallStarted {
                tool_call_id,
                tool_name,
                step,
            } => {
                write!(
                    f,
                    "ToolCallStarted step={step} tool={tool_name} id={tool_call_id}"
                )
            }
            Self::ToolCallFinished {
                tool_call_id,
                tool_name,
                duration,
                success,
                output_size_bytes,
            } => {
                write!(
                    f,
                    "ToolCallFinished tool={tool_name} id={tool_call_id} duration={}ms ok={success} output={output_size_bytes}B",
                    duration.as_millis()
                )
            }
            Self::ToolCallRejected {
                tool_call_id,
                tool_name,
                reason,
            } => {
                write!(
                    f,
                    "ToolCallRejected tool={tool_name} id={tool_call_id} reason=\"{reason}\""
                )
            }
            Self::StreamingSummary {
                step,
                content_chunks,
                reasoning_chunks,
            } => {
                write!(
                    f,
                    "StreamingSummary step={step} content={content_chunks} reasoning={reasoning_chunks}"
                )
            }
            Self::SubagentFinished {
                description,
                steps,
                llm_calls,
                tool_calls,
                usage,
                duration,
            } => {
                write!(
                    f,
                    "SubagentFinished desc=\"{description}\" steps={steps} llm={llm_calls} tools={tool_calls} tokens={} duration={}ms",
                    usage.total_tokens,
                    duration.as_millis()
                )
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_run_started() {
        let e = TraceEvent::RunStarted {
            session_id: "s1".into(),
            user_input: "hello world".into(),
            max_steps: 50,
            max_retries: 3,
        };
        let s = e.to_string();
        assert!(s.starts_with("RunStarted"));
        assert!(s.contains("session=s1"));
        assert!(s.contains("input=\"hello world\""));
        assert!(s.contains("max_steps=50"));
        assert!(s.contains("max_retries=3"));
    }

    #[test]
    fn test_display_run_finished() {
        let e = TraceEvent::RunFinished {
            outcome: "success".into(),
            total_duration: Duration::from_millis(1234),
            total_steps: 3,
            total_llm_calls: 4,
            total_tool_calls: 5,
            cumulative_usage: Usage {
                prompt_tokens: 100,
                completion_tokens: 200,
                total_tokens: 300,
            },
        };
        let s = e.to_string();
        assert!(s.starts_with("RunFinished"));
        assert!(s.contains("duration=1234ms"));
        assert!(s.contains("steps=3"));
        assert!(s.contains("tokens=300"));
    }

    #[test]
    fn test_display_step_started() {
        let s = TraceEvent::StepStarted { step: 42 }.to_string();
        assert_eq!(s, "StepStarted step=42");
    }

    #[test]
    fn test_display_llm_call_failed() {
        let e = TraceEvent::LlmCallFailed {
            step: 2,
            attempt: 1,
            error: "timeout".into(),
            will_retry: true,
            duration: Duration::from_secs(30),
        };
        let s = e.to_string();
        assert!(s.contains("error=\"timeout\""));
        assert!(s.contains("retry=true"));
    }

    #[test]
    fn test_display_tool_call_finished() {
        let e = TraceEvent::ToolCallFinished {
            tool_call_id: "call_123".into(),
            tool_name: "read".into(),
            duration: Duration::from_millis(50),
            success: true,
            output_size_bytes: 1024,
        };
        let s = e.to_string();
        assert!(s.contains("tool=read"));
        assert!(s.contains("ok=true"));
        assert!(s.contains("output=1024B"));
    }
}
