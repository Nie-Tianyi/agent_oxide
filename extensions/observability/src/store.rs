//! Trace event storage and aggregated run metrics.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use provider::Usage;

use crate::event::TraceEvent;

// ── RunMetrics ────────────────────────────────────────────────────────────────────

/// Aggregated metrics for the current (or most recent) agent run.
///
/// Counters use atomics for lock-free increments from the hot path.
/// Vec fields (per-step data) use a `Mutex` for occasional writes.
/// Read by the TUI status bar each frame.
#[derive(Debug)]
pub struct RunMetrics {
    // ── Counters (lock-free) ──
    pub total_llm_calls: AtomicUsize,
    pub total_llm_retries: AtomicUsize,
    pub total_tool_calls: AtomicUsize,
    pub total_tool_errors: AtomicUsize,
    pub total_tool_rejections: AtomicUsize,
    pub cumulative_prompt_tokens: AtomicU32,
    pub cumulative_completion_tokens: AtomicU32,
    pub cumulative_total_tokens: AtomicU32,
    pub step_count: AtomicUsize,

    // ── Timing (lock-free, written once) ──
    pub is_streaming: AtomicBool,
    pub run_started: AtomicBool,

    // ── Vec fields (Mutex-protected, infrequent writes) ──
    per_step_tokens: Mutex<Vec<(usize, Usage)>>,
    per_step_timings: Mutex<Vec<(usize, std::time::Duration)>>,
    per_tool_timings: Mutex<Vec<(String, std::time::Duration)>>,
    /// Error messages from the current run.
    errors: Mutex<Vec<String>>,
    /// Description of the most recent subagent finish.
    subagent_descriptions: Mutex<Vec<String>>,
}

impl Default for RunMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl RunMetrics {
    pub fn new() -> Self {
        Self {
            total_llm_calls: AtomicUsize::new(0),
            total_llm_retries: AtomicUsize::new(0),
            total_tool_calls: AtomicUsize::new(0),
            total_tool_errors: AtomicUsize::new(0),
            total_tool_rejections: AtomicUsize::new(0),
            cumulative_prompt_tokens: AtomicU32::new(0),
            cumulative_completion_tokens: AtomicU32::new(0),
            cumulative_total_tokens: AtomicU32::new(0),
            step_count: AtomicUsize::new(0),
            is_streaming: AtomicBool::new(false),
            run_started: AtomicBool::new(false),
            per_step_tokens: Mutex::new(Vec::new()),
            per_step_timings: Mutex::new(Vec::new()),
            per_tool_timings: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
            subagent_descriptions: Mutex::new(Vec::new()),
        }
    }

    pub fn add_token_usage(&self, usage: &Usage) {
        self.cumulative_prompt_tokens
            .fetch_add(usage.prompt_tokens, Ordering::Relaxed);
        self.cumulative_completion_tokens
            .fetch_add(usage.completion_tokens, Ordering::Relaxed);
        self.cumulative_total_tokens
            .fetch_add(usage.total_tokens, Ordering::Relaxed);
    }

    pub fn add_step_tokens(&self, step: usize, usage: Usage) {
        if let Ok(mut v) = self.per_step_tokens.lock() {
            v.push((step, usage));
        }
    }

    pub fn add_step_timing(&self, step: usize, duration: std::time::Duration) {
        if let Ok(mut v) = self.per_step_timings.lock() {
            v.push((step, duration));
        }
    }

    pub fn add_tool_timing(&self, name: &str, duration: std::time::Duration) {
        if let Ok(mut v) = self.per_tool_timings.lock() {
            v.push((name.to_string(), duration));
        }
    }

    pub fn add_error(&self, error: String) {
        if let Ok(mut v) = self.errors.lock() {
            v.push(error);
        }
    }

    pub fn add_subagent(&self, description: String) {
        if let Ok(mut v) = self.subagent_descriptions.lock() {
            v.push(description);
        }
    }

    /// Reset all accumulators for a new run.
    pub fn reset(&self) {
        self.total_llm_calls.store(0, Ordering::Relaxed);
        self.total_llm_retries.store(0, Ordering::Relaxed);
        self.total_tool_calls.store(0, Ordering::Relaxed);
        self.total_tool_errors.store(0, Ordering::Relaxed);
        self.total_tool_rejections.store(0, Ordering::Relaxed);
        self.cumulative_prompt_tokens.store(0, Ordering::Relaxed);
        self.cumulative_completion_tokens
            .store(0, Ordering::Relaxed);
        self.cumulative_total_tokens.store(0, Ordering::Relaxed);
        self.step_count.store(0, Ordering::Relaxed);
        self.is_streaming.store(false, Ordering::Relaxed);
        self.run_started.store(false, Ordering::Relaxed);
        if let Ok(mut v) = self.per_step_tokens.lock() {
            v.clear();
        }
        if let Ok(mut v) = self.per_step_timings.lock() {
            v.clear();
        }
        if let Ok(mut v) = self.per_tool_timings.lock() {
            v.clear();
        }
        if let Ok(mut v) = self.errors.lock() {
            v.clear();
        }
        if let Ok(mut v) = self.subagent_descriptions.lock() {
            v.clear();
        }
    }

    // ── Snapshot helpers (for TUI display) ──

    /// Number of LLM calls (excluding retries).
    pub fn llm_calls(&self) -> usize {
        let total = self.total_llm_calls.load(Ordering::Relaxed);
        let retries = self.total_llm_retries.load(Ordering::Relaxed);
        total.saturating_sub(retries)
    }

    pub fn llm_calls_total(&self) -> usize {
        self.total_llm_calls.load(Ordering::Relaxed)
    }

    pub fn tool_calls(&self) -> usize {
        self.total_tool_calls.load(Ordering::Relaxed)
    }

    pub fn tool_errors(&self) -> usize {
        self.total_tool_errors.load(Ordering::Relaxed)
    }

    pub fn tool_rejections(&self) -> usize {
        self.total_tool_rejections.load(Ordering::Relaxed)
    }

    pub fn steps(&self) -> usize {
        self.step_count.load(Ordering::Relaxed)
    }

    pub fn streaming(&self) -> bool {
        self.is_streaming.load(Ordering::Relaxed)
    }

    pub fn total_tokens(&self) -> u32 {
        self.cumulative_total_tokens.load(Ordering::Relaxed)
    }
}

// ── TraceStore ────────────────────────────────────────────────────────────────────

/// Central trace event dispatcher.
///
/// Emits each event to the [`tracing`] infrastructure (which writes to the
/// file log via `tracing_appender`) and exposes [`RunMetrics`] for the TUI
/// status bar.
///
/// Shared between the agent task (writes) and the TUI loop (reads metrics).
pub struct TraceStore {
    pub metrics: RunMetrics,
}

impl Default for TraceStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceStore {
    pub fn new() -> Self {
        Self {
            metrics: RunMetrics::new(),
        }
    }

    /// Emit a trace event.
    ///
    /// Dispatches to the [`tracing`] crate at the appropriate level for the
    /// event variant.  All events use `target: "agent"` for fine-grained
    /// filtering (e.g. `AGENT_LOG=agent=debug`).
    ///
    /// * `INFO`  — major lifecycle events (run start/finish, subagent finish)
    /// * `WARN`  — error conditions (LLM call failed, tool rejected)
    /// * `DEBUG` — step-by-step events (step start, LLM/tool call boundaries)
    /// * `TRACE` — streaming chunk summaries
    ///
    /// The [`Display`](std::fmt::Display) impl on [`TraceEvent`] is used for
    /// formatting, and is only evaluated when the tracing filter enables the
    /// corresponding level for the `agent` target (lazy evaluation).
    pub fn emit(&self, event: TraceEvent) {
        // Match on event variant to select the appropriate tracing macro.
        // Each tracing macro requires a compile-time-constant level, so we
        // can't use a runtime `tracing::Level` variable with `tracing::event!`.
        //
        // Using `message = %event` ensures lazy evaluation: the Display impl
        // is only called when the tracing filter enables this target+level.
        match &event {
            TraceEvent::LlmCallFailed { .. } | TraceEvent::ToolCallRejected { .. } => {
                tracing::warn!(target: "agent", message = %event);
            }
            TraceEvent::RunStarted { .. }
            | TraceEvent::RunFinished { .. }
            | TraceEvent::SubagentFinished { .. } => {
                tracing::info!(target: "agent", message = %event);
            }
            TraceEvent::StreamingSummary { .. } => {
                tracing::trace!(target: "agent", message = %event);
            }
            _ => {
                tracing::debug!(target: "agent", message = %event);
            }
        }
    }

    /// Reset metrics for a new run.
    pub fn reset_metrics(&self) {
        self.metrics.reset();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_counters() {
        let m = RunMetrics::new();
        m.add_token_usage(&Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        });
        m.add_token_usage(&Usage {
            prompt_tokens: 200,
            completion_tokens: 60,
            total_tokens: 260,
        });
        assert_eq!(m.total_tokens(), 410);
        m.reset();
        assert_eq!(m.total_tokens(), 0);
    }
}
