//! Thread-safe trace event storage with a lock-free ring buffer for the hot path.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

use provider::Usage;

use crate::event::{Timestamped, TraceEvent};

// ── Ring buffer ──────────────────────────────────────────────────────────────────

/// Fixed-size ring buffer for trace events.
///
/// Lock-free writes via a [`Mutex`] (held briefly, no allocation).
/// Overflow wraps silently; dropped events are tracked via `lost_events`.
const RING_CAPACITY: usize = 4096;

struct RingBuf {
    buf: Vec<Option<Timestamped<TraceEvent>>>,
    head: usize, // next write position
    len: usize,  // number of valid entries (≤ capacity)
    /// True if overflow has occurred at least once since last drain.
    wrapped: bool,
    /// Number of events dropped due to overflow since last drain.
    lost_events: usize,
}

impl RingBuf {
    fn new() -> Self {
        let mut buf = Vec::with_capacity(RING_CAPACITY);
        buf.resize_with(RING_CAPACITY, || None);
        Self {
            buf,
            head: 0,
            len: 0,
            wrapped: false,
            lost_events: 0,
        }
    }

    /// Push an event. O(1), no allocation.
    /// Returns `true` if an old event was overwritten.
    fn push(&mut self, event: Timestamped<TraceEvent>) {
        self.buf[self.head] = Some(event);
        self.head = (self.head + 1) % RING_CAPACITY;
        if self.len < RING_CAPACITY {
            self.len += 1;
        } else {
            // Overflow — we've overwritten the oldest entry.
            self.wrapped = true;
            self.lost_events += 1;
        }
    }

    /// Drain all events into a `Vec`, resetting the buffer.
    /// Returns events in insertion order (oldest first).
    fn drain(&mut self) -> Vec<Timestamped<TraceEvent>> {
        let mut out = Vec::with_capacity(self.len);

        if self.wrapped {
            // Wrapped: collect from head..end then 0..head.
            for i in self.head..RING_CAPACITY {
                if let Some(evt) = self.buf[i].take() {
                    out.push(evt);
                }
            }
            for i in 0..self.head {
                if let Some(evt) = self.buf[i].take() {
                    out.push(evt);
                }
            }
        } else {
            // No wrap: collect from 0..len.
            for i in 0..self.len {
                if let Some(evt) = self.buf[i].take() {
                    out.push(evt);
                }
            }
        }

        self.head = 0;
        self.len = 0;
        self.wrapped = false;
        self.lost_events = 0;
        out
    }

    fn len(&self) -> usize {
        self.len
    }
}

// ── RunMetrics ────────────────────────────────────────────────────────────────────

/// Aggregated metrics for the current (or most recent) agent run.
///
/// Counters use atomics for lock-free increments from the hot path.
/// Vec fields (per-step data) use a `Mutex` for occasional writes.
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
        // Clear Vecs
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

/// Central trace event collector.
///
/// Shared between the agent task (writes) and the TUI loop (reads).
/// Uses a ring buffer for low-overhead writes on the hot path and
/// atomically-updated [`RunMetrics`] for lock-free metric reads.
pub struct TraceStore {
    ring: Mutex<RingBuf>,
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
            ring: Mutex::new(RingBuf::new()),
            metrics: RunMetrics::new(),
        }
    }

    /// Emit a trace event.
    ///
    /// This is called from hook methods on the agent task.  The ring buffer
    /// is held briefly (no allocation).  O(1).
    pub fn emit(&self, event: TraceEvent) {
        let ts = Timestamped::new(event);
        if let Ok(mut ring) = self.ring.lock() {
            ring.push(ts);
        }
    }

    /// Drain all buffered events since the last call.
    ///
    /// Called once per render frame from the TUI event loop (~20fps).
    /// Returns events in insertion order.
    pub fn drain_events(&self) -> Vec<Timestamped<TraceEvent>> {
        self.ring
            .lock()
            .map(|mut ring| ring.drain())
            .unwrap_or_default()
    }

    /// Number of buffered (not yet drained) events.
    pub fn buffered_len(&self) -> usize {
        self.ring.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// Reset metrics for a new run.
    pub fn reset_metrics(&self) {
        self.metrics.reset();
    }

    /// Export all currently-buffered events as JSONL.
    ///
    /// Drains the ring buffer and writes one JSON object per line.
    /// Returns the number of events written.
    pub fn export_jsonl(&self, writer: &mut impl std::io::Write) -> std::io::Result<usize> {
        let events = self.drain_events();
        let count = events.len();
        for evt in &events {
            let json = serde_json::to_string(&TraceEventRecord::from(evt))
                .map_err(std::io::Error::other)?;
            writeln!(writer, "{json}")?;
        }
        Ok(count)
    }
}

// ── JSONL serialization helper ─────────────────────────────────────────────────────

/// Plain-data record for JSONL export.
/// Mirrors [`TraceEvent`] without the [`Timestamped`] wrapper.
#[derive(serde::Serialize)]
struct TraceEventRecord {
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl TraceEventRecord {
    fn from(ts: &Timestamped<TraceEvent>) -> Self {
        use TraceEvent::*;
        let mut rec = Self {
            event: String::new(),
            session_id: None,
            step: None,
            attempt: None,
            duration_ms: None,
            tokens_total: None,
            tool_name: None,
            tool_call_id: None,
            detail: None,
        };
        match &ts.inner {
            RunStarted { session_id, user_input, .. } => {
                rec.event = "RunStarted".into();
                rec.session_id = Some(session_id.clone());
                rec.detail = Some(user_input.clone());
            }
            RunFinished { outcome, total_duration, total_steps, total_llm_calls, total_tool_calls, cumulative_usage } => {
                rec.event = "RunFinished".into();
                rec.duration_ms = Some(total_duration.as_millis() as u64);
                rec.tokens_total = Some(cumulative_usage.total_tokens);
                rec.detail = Some(format!(
                    "outcome={outcome} steps={total_steps} llm={total_llm_calls} tools={total_tool_calls}"
                ));
            }
            StepStarted { step } => {
                rec.event = "StepStarted".into();
                rec.step = Some(*step);
            }
            LlmCallStarted { step, attempt, message_count } => {
                rec.event = "LlmCallStarted".into();
                rec.step = Some(*step);
                rec.attempt = Some(*attempt);
                rec.detail = Some(format!("msgs={message_count}"));
            }
            LlmCallFinished { step, attempt, duration, usage, finish_reason } => {
                rec.event = "LlmCallFinished".into();
                rec.step = Some(*step);
                rec.attempt = Some(*attempt);
                rec.duration_ms = Some(duration.as_millis() as u64);
                rec.tokens_total = Some(usage.total_tokens);
                rec.detail = finish_reason.clone();
            }
            LlmCallFailed { step, attempt, error, will_retry, duration } => {
                rec.event = "LlmCallFailed".into();
                rec.step = Some(*step);
                rec.attempt = Some(*attempt);
                rec.duration_ms = Some(duration.as_millis() as u64);
                rec.detail = Some(format!("error={error} retry={will_retry}"));
            }
            ToolCallStarted { tool_call_id, tool_name, step } => {
                rec.event = "ToolCallStarted".into();
                rec.step = Some(*step);
                rec.tool_name = Some(tool_name.clone());
                rec.tool_call_id = Some(tool_call_id.clone());
            }
            ToolCallFinished { tool_call_id, tool_name, duration, success, output_size_bytes } => {
                rec.event = "ToolCallFinished".into();
                rec.tool_name = Some(tool_name.clone());
                rec.tool_call_id = Some(tool_call_id.clone());
                rec.duration_ms = Some(duration.as_millis() as u64);
                rec.detail = Some(format!("success={success} output_bytes={output_size_bytes}"));
            }
            ToolCallRejected { tool_call_id, tool_name, reason } => {
                rec.event = "ToolCallRejected".into();
                rec.tool_name = Some(tool_name.clone());
                rec.tool_call_id = Some(tool_call_id.clone());
                rec.detail = Some(reason.clone());
            }
            StreamingSummary { step, content_chunks, reasoning_chunks } => {
                rec.event = "StreamingSummary".into();
                rec.step = Some(*step);
                rec.detail = Some(format!("content_chunks={content_chunks} reasoning_chunks={reasoning_chunks}"));
            }
            SubagentFinished { description, steps, llm_calls, tool_calls, usage, duration } => {
                rec.event = "SubagentFinished".into();
                rec.duration_ms = Some(duration.as_millis() as u64);
                rec.tokens_total = Some(usage.total_tokens);
                rec.detail = Some(format!(
                    "desc={description} steps={steps} llm={llm_calls} tools={tool_calls}"
                ));
            }
        }
        rec
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::TraceEvent;

    #[test]
    fn test_ring_buf_push_and_drain() {
        let mut ring = RingBuf::new();
        for i in 0..10 {
            ring.push(Timestamped::new(TraceEvent::StepStarted { step: i }));
        }
        assert_eq!(ring.len(), 10);
        let drained = ring.drain();
        assert_eq!(drained.len(), 10);
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn test_ring_buf_overflow_wraps() {
        let mut ring = RingBuf::new();
        // Fill the buffer and overflow by 2
        let total = RING_CAPACITY + 2;
        for i in 0..total {
            ring.push(Timestamped::new(TraceEvent::StepStarted { step: i }));
        }
        assert_eq!(ring.len(), RING_CAPACITY);
        assert!(ring.lost_events > 0);
        let drained = ring.drain();
        assert_eq!(drained.len(), RING_CAPACITY);
        // First event should be index 2 (oldest not overwritten)
        if let TraceEvent::StepStarted { step } = &drained[0].inner {
            assert_eq!(*step, 2);
        } else {
            panic!("unexpected event variant");
        }
    }

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

    #[test]
    fn test_trace_store_emit_and_drain() {
        let store = TraceStore::new();
        store.emit(TraceEvent::StepStarted { step: 1 });
        store.emit(TraceEvent::StepStarted { step: 2 });
        let events = store.drain_events();
        assert_eq!(events.len(), 2);
        // Second drain should be empty
        let events2 = store.drain_events();
        assert!(events2.is_empty());
    }
}
