//! Observability hook — captures full-chain trace events via the [`AgentHook`] interface.
//!
//! This hook instruments every phase of the agent lifecycle: run start/finish,
//! step transitions, LLM calls (start/end/error), and tool execution (start/end/reject).
//! Events are dispatched to the [`tracing`] infrastructure via [`TraceStore::emit`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use engine::{AgentError, AgentHook};
use memory::SharedMemory;
use provider::{Message, ProviderError, ToolCall};

use crate::{TraceEvent, TraceStore};

/// An [`AgentHook`] that records trace events into a shared [`TraceStore`].
///
/// The hook holds its own clone of [`SharedMemory`] so it can read
/// `last_usage` during [`on_llm_end`](AgentHook::on_llm_end) (which only
/// receives `&Message`, not `&SharedMemory`).
pub struct ObservabilityHook {
    store: Arc<TraceStore>,
    memory: SharedMemory,

    /// Captured at `on_run_start` for computing total run duration.
    run_start: Mutex<Option<Instant>>,
    /// Captured at `on_llm_start` for computing LLM call duration.
    llm_start: Mutex<Option<Instant>>,
    /// Captured at `before_tool_call` for computing tool execution duration,
    /// keyed by tool call ID.
    tool_starts: Mutex<HashMap<String, Instant>>,

    /// 1-indexed current step number.
    current_step: AtomicUsize,
    /// Total LLM calls (including retries) in the current run.
    llm_call_count: AtomicUsize,
    /// Total tool calls (excluding rejections) in the current run.
    tool_call_count: AtomicUsize,
    /// Total LLM errors in the current run.
    llm_error_count: AtomicUsize,
    /// Total tool rejections in the current run.
    tool_rejection_count: AtomicUsize,
}

impl ObservabilityHook {
    pub fn new(store: Arc<TraceStore>, memory: SharedMemory) -> Self {
        Self {
            store,
            memory,
            run_start: Mutex::new(None),
            llm_start: Mutex::new(None),
            tool_starts: Mutex::new(HashMap::new()),
            current_step: AtomicUsize::new(0),
            llm_call_count: AtomicUsize::new(0),
            tool_call_count: AtomicUsize::new(0),
            llm_error_count: AtomicUsize::new(0),
            tool_rejection_count: AtomicUsize::new(0),
        }
    }

    /// Convenience — emit a trace event and update metrics atomically.
    fn emit(&self, event: TraceEvent) {
        self.store.emit(event);
    }
}

impl AgentHook for ObservabilityHook {
    // ── Run lifecycle ──────────────────────────────────────────────────────────

    fn on_run_start(&self, session_id: &str, user_input: &str, _memory: &SharedMemory) {
        // Reset per-run accumulators.
        self.store.reset_metrics();
        self.llm_call_count.store(0, Ordering::Relaxed);
        self.tool_call_count.store(0, Ordering::Relaxed);
        self.llm_error_count.store(0, Ordering::Relaxed);
        self.tool_rejection_count.store(0, Ordering::Relaxed);
        self.current_step.store(0, Ordering::Relaxed);
        if let Ok(mut ts) = self.tool_starts.lock() {
            ts.clear();
        }

        let now = Instant::now();
        if let Ok(mut rs) = self.run_start.lock() {
            *rs = Some(now);
        }
        self.store
            .metrics
            .is_streaming
            .store(true, Ordering::Relaxed);
        self.store
            .metrics
            .run_started
            .store(true, Ordering::Relaxed);

        // Truncate user input to 200 chars for storage efficiency,
        // using floor_char_boundary to avoid splitting multi-byte UTF-8 chars.
        let truncated = if user_input.len() > 200 {
            let boundary = user_input.floor_char_boundary(200);
            let mut s = user_input[..boundary].to_string();
            s.push('…');
            s
        } else {
            user_input.to_string()
        };

        self.emit(TraceEvent::RunStarted {
            session_id: session_id.to_string(),
            user_input: truncated,
            max_steps: 0,
            max_retries: 0,
        });
    }

    fn on_run_finish(
        &self,
        _session_id: &str,
        outcome: &engine::RunOutcome,
        _memory: &SharedMemory,
    ) {
        let duration = self
            .run_start
            .lock()
            .ok()
            .and_then(|rs| *rs)
            .map(|start| start.elapsed())
            .unwrap_or_default();

        let outcome_str = match outcome {
            engine::RunOutcome::Success { answer: _ } => "success".to_string(),
            engine::RunOutcome::Error { error } => format!("error: {error}"),
            engine::RunOutcome::Cancelled => "cancelled".to_string(),
        };

        let cumulative_usage = {
            let usage = self.memory.read().expect("memory lock poisoned");
            let history = &usage.usage_history;
            if history.is_empty() {
                usage.last_usage.clone().unwrap_or(provider::Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                })
            } else {
                let mut total = provider::Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                };
                for u in history {
                    total.prompt_tokens += u.prompt_tokens;
                    total.completion_tokens += u.completion_tokens;
                    total.total_tokens += u.total_tokens;
                }
                total
            }
        };

        self.emit(TraceEvent::RunFinished {
            outcome: outcome_str,
            total_duration: duration,
            total_steps: self.current_step.load(Ordering::Relaxed),
            total_llm_calls: self.llm_call_count.load(Ordering::Relaxed),
            total_tool_calls: self.tool_call_count.load(Ordering::Relaxed),
            cumulative_usage,
        });

        self.store
            .metrics
            .is_streaming
            .store(false, Ordering::Relaxed);
    }

    // ── Step lifecycle ─────────────────────────────────────────────────────────

    fn on_step_start(&self, _session_id: &str, step: usize, _max_steps: usize) {
        self.current_step.store(step, Ordering::Relaxed);
        self.store.metrics.step_count.store(step, Ordering::Relaxed);
        self.emit(TraceEvent::StepStarted { step });
    }

    // ── LLM lifecycle ──────────────────────────────────────────────────────────

    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let step = self.current_step.load(Ordering::Relaxed);
        let message_count = memory.read().map(|m| m.len()).unwrap_or(0);

        let now = Instant::now();
        if let Ok(mut ls) = self.llm_start.lock() {
            *ls = Some(now);
        }

        self.emit(TraceEvent::LlmCallStarted {
            step,
            attempt: 0,
            message_count,
        });
    }

    fn on_llm_end(&self, _session_id: &str, _response: &Message) {
        let step = self.current_step.load(Ordering::Relaxed);
        let duration = self
            .llm_start
            .lock()
            .ok()
            .and_then(|ls| *ls)
            .map(|start| start.elapsed())
            .unwrap_or_default();

        let usage = self
            .memory
            .read()
            .expect("memory lock poisoned")
            .last_usage
            .clone()
            .unwrap_or(provider::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            });

        self.store.metrics.add_token_usage(&usage);
        self.store.metrics.add_step_tokens(step, usage.clone());

        self.llm_call_count.fetch_add(1, Ordering::Relaxed);
        self.store
            .metrics
            .total_llm_calls
            .fetch_add(1, Ordering::Relaxed);

        let finish_reason = _response
            .tool_calls
            .as_ref()
            .map(|tcs| if tcs.is_empty() { "stop" } else { "tool_calls" })
            .or(Some("stop"))
            .map(|s| s.to_string());

        self.emit(TraceEvent::LlmCallFinished {
            step,
            attempt: 0,
            duration,
            usage,
            finish_reason,
        });
    }

    fn on_llm_error(
        &self,
        _session_id: &str,
        error: &ProviderError,
        attempt: usize,
        will_retry: bool,
    ) {
        let step = self.current_step.load(Ordering::Relaxed);
        let duration = self
            .llm_start
            .lock()
            .ok()
            .and_then(|ls| *ls)
            .map(|start| start.elapsed())
            .unwrap_or_default();

        self.llm_call_count.fetch_add(1, Ordering::Relaxed);
        self.llm_error_count.fetch_add(1, Ordering::Relaxed);
        self.store
            .metrics
            .total_llm_calls
            .fetch_add(1, Ordering::Relaxed);
        if will_retry {
            self.store
                .metrics
                .total_llm_retries
                .fetch_add(1, Ordering::Relaxed);
        }
        self.store.metrics.add_error(error.to_string());

        self.emit(TraceEvent::LlmCallFailed {
            step,
            attempt,
            error: error.to_string(),
            will_retry,
            duration,
        });
    }

    // ── Tool lifecycle ─────────────────────────────────────────────────────────

    fn before_tool_call(&self, _session_id: &str, tool_call: &ToolCall) -> Result<(), AgentError> {
        let step = self.current_step.load(Ordering::Relaxed);

        if let Ok(mut ts) = self.tool_starts.lock() {
            ts.insert(tool_call.id.clone(), Instant::now());
        }

        self.emit(TraceEvent::ToolCallStarted {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.function.name.clone(),
            step,
        });

        Ok(())
    }

    fn after_tool_call(&self, _session_id: &str, tool_call: &ToolCall, observation: &str) {
        let duration = self
            .tool_starts
            .lock()
            .ok()
            .and_then(|mut ts| ts.remove(&tool_call.id))
            .map(|start| start.elapsed())
            .unwrap_or_default();

        self.tool_call_count.fetch_add(1, Ordering::Relaxed);
        self.store
            .metrics
            .total_tool_calls
            .fetch_add(1, Ordering::Relaxed);
        self.store
            .metrics
            .add_tool_timing(&tool_call.function.name, duration);

        self.emit(TraceEvent::ToolCallFinished {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.function.name.clone(),
            duration,
            success: true,
            output_size_bytes: observation.len(),
        });
    }

    fn on_tool_failed(&self, _session_id: &str, tool_call: &ToolCall, _error: &str) {
        let duration = self
            .tool_starts
            .lock()
            .ok()
            .and_then(|mut ts| ts.remove(&tool_call.id))
            .map(|start| start.elapsed())
            .unwrap_or_default();

        self.tool_call_count.fetch_add(1, Ordering::Relaxed);
        self.store
            .metrics
            .total_tool_calls
            .fetch_add(1, Ordering::Relaxed);
        self.store
            .metrics
            .total_tool_errors
            .fetch_add(1, Ordering::Relaxed);

        self.emit(TraceEvent::ToolCallFinished {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.function.name.clone(),
            duration,
            success: false,
            output_size_bytes: 0,
        });
    }
}
