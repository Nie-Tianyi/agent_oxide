//! [`SubagentTool`] — a `Tool` that spawns a child [`Agent`] for complex sub-tasks.
//!
//! When the parent LLM calls the `task` tool, this implementation creates a
//! fresh agent with its own memory and a filtered tool set, runs it to
//! completion, and streams progress events back to the parent via
//! [`ProgressStream`].

use std::sync::Arc;
use std::time::Instant;

use engine::{Agent, EngineContext};
use memory::{Memory, SharedMemory};
use observability::{TraceEvent, TraceStore};
use provider::{LLMClient, Message, Role};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tools::{Progress, ProgressStream, ToolError, ToolRegistry, tool};

use crate::config::SubagentConfig;

/// Default timeout in seconds when `SubagentConfig::timeout_secs` is `None`.
const DEFAULT_SUBAGENT_TIMEOUT_SECS: u64 = 300;
/// Maximum chars of tool arguments to show in progress messages before truncating.
const TRUNCATE_ARGS_CHARS: usize = 120;
/// Maximum chars of tool output to show in summary before truncating.
const OUTPUT_SUMMARY_CHARS: usize = 160;

// ── SubagentTool ──────────────────────────────────────────────────────────────

/// A tool that spawns a fresh sub-agent to complete a complex sub-task.
///
/// Generic over `C` — any LLM client that is cloneable.  Each invocation
/// clones the client and runs a new [`Agent`].
#[tool(
    name = "task",
    description = "Delegate a complex task to a sub-agent with read-only workspace tools (read, ls, glob, grep, calculator). The sub-agent works independently — it can investigate, search, and analyze, but cannot write, edit, or execute shell commands. Use this for multi-step tasks requiring multiple tool calls and independent reasoning. Provide a clear description and a detailed prompt with specific instructions about the expected output format.",
    args = TaskArgs
)]
pub struct SubagentTool<C: LLMClient + Clone + 'static> {
    llm: C,
    config: SubagentConfig,
    subagent_tools: Arc<ToolRegistry>,
    parent_memory: SharedMemory,
    /// Optional trace store — when set, subagent runs emit
    /// [`TraceEvent::SubagentFinished`] on completion.
    trace_store: Option<Arc<TraceStore>>,
}

impl<C: LLMClient + Clone + 'static> SubagentTool<C> {
    /// Create a new subagent tool.
    ///
    /// * `llm` — A cloneable LLM client.  Each sub-agent invocation creates
    ///   a fresh clone so concurrent sub-agents are independent.
    /// * `config` — Sub-agent policy (model, max steps, timeout, …).
    /// * `subagent_tools` — The tool registry given to the child agent.
    ///   Should be a **subset** of the parent's tools, without the `task`
    ///   tool itself (to prevent infinite recursion).
    /// * `parent_memory` — Reference to the parent agent's conversation
    ///   memory.  Used only for reading optional context inheritance;
    ///   the sub-agent's own memory is always isolated.
    pub fn new(
        llm: C,
        config: SubagentConfig,
        subagent_tools: Arc<ToolRegistry>,
        parent_memory: SharedMemory,
    ) -> Self {
        Self {
            llm,
            config,
            subagent_tools,
            parent_memory,
            trace_store: None,
        }
    }

    /// Attach a trace store for subagent observability.
    pub fn with_trace_store(mut self, store: Arc<TraceStore>) -> Self {
        self.trace_store = Some(store);
        self
    }
}

// ── TaskArgs ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskArgs {
    /// High-level description for progress reporting.
    description: String,
    /// The full prompt passed to the sub-agent.
    prompt: String,
}

// ── execute_stream (called by the #[tool] macro) ─────────────────────────────

impl<C: LLMClient + Clone + 'static> SubagentTool<C> {
    fn execute_stream(&self, args: TaskArgs) -> Result<ProgressStream, ToolError> {
        // Channel: the spawned task sends Progress events here, and the
        // wrapping stream yields them to the parent agent's tool loop.
        let (progress_tx, progress_rx) = mpsc::unbounded_channel::<Progress>();

        // Clone everything the async task needs.
        let llm = self.llm.clone();
        let config = self.config.clone();
        let subagent_tools = Arc::clone(&self.subagent_tools);
        let parent_memory = Arc::clone(&self.parent_memory);
        let trace_store = self.trace_store.clone();
        let description = args.description;
        let prompt = args.prompt;

        // Spawn the sub-agent on the current tokio runtime.
        // `execute_stream` is called from within the async agent loop,
        // so `tokio::spawn` always has an active runtime context.
        tokio::spawn(async move {
            run_subagent(
                llm,
                config,
                subagent_tools,
                parent_memory,
                trace_store,
                description,
                prompt,
                progress_tx,
            )
            .await;
        });

        // Return a ProgressStream backed by the channel receiver.
        let stream = futures_util::stream::unfold(progress_rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(ProgressStream::new(Box::pin(stream)))
    }
}

// ── Async runner ─────────────────────────────────────────────────────────────

async fn run_subagent<C: LLMClient + 'static>(
    llm: C,
    config: SubagentConfig,
    subagent_tools: Arc<ToolRegistry>,
    parent_memory: SharedMemory,
    trace_store: Option<Arc<TraceStore>>,
    description: String,
    prompt: String,
    progress_tx: mpsc::UnboundedSender<Progress>,
) {
    let start = Instant::now();

    // 1. Build fresh, isolated memory (user prompt is pushed automatically
    //    by `run_with_events` when the agent loop starts).
    let memory = build_subagent_memory(&config, &parent_memory);

    // 2. Build EngineContext for the sub-agent.
    let ctx = EngineContext::builder(llm, memory.clone(), subagent_tools, &config.model)
        .max_steps(config.max_steps)
        .max_retries(config.max_retries)
        .streaming(config.streaming)
        .build();
    let agent = Agent::new(ctx);

    // 3. Channel for sub-agent events.
    let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();

    // 4. Notify the parent: task started.
    let _ = progress_tx.send(Progress::InProgress(format!(
        "⚙ Starting sub-agent: {description}"
    )));

    // 5. Spawn the agent loop in its own task.
    let agent_handle = tokio::spawn(async move { agent.run_with_events(&prompt, sub_tx).await });

    // 6. Drive event forwarding, racing against timeout.
    let timeout = config
        .timeout_secs
        .map(std::time::Duration::from_secs)
        .unwrap_or_else(|| std::time::Duration::from_secs(DEFAULT_SUBAGENT_TIMEOUT_SECS));
    let deadline = tokio::time::Instant::now() + timeout;

    let mut tool_call_count: usize = 0;
    let mut llm_call_count: usize = 0;

    let result = loop {
        tokio::select! {
            event = sub_rx.recv() => {
                match event {
                    Some(evt) => {
                        // Count tool calls and LLM calls for trace.
                        if matches!(&evt, engine::AgentEvent::ToolCall { .. }) {
                            tool_call_count += 1;
                        }
                        if matches!(&evt, engine::AgentEvent::RunCompleted { .. }
                            | engine::AgentEvent::RunFailed { .. })
                        {
                            llm_call_count += 1; // rough: RunCompleted follows an LLM call
                        }
                        forward_event_to_progress(evt, &progress_tx);
                    }
                    None => {
                        // Channel closed — agent task finished or panicked.
                        break agent_handle.await;
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                agent_handle.abort();
                let _ = progress_tx.send(Progress::Done(format!(
                    "Sub-agent timed out after {:.0}s",
                    timeout.as_secs_f64()
                )));
                // Emit trace even on timeout
                emit_subagent_trace(&trace_store, &description, start, &memory, tool_call_count, llm_call_count);
                return;
            }
        }
    };

    // 7. Emit subagent trace before the final result.
    emit_subagent_trace(&trace_store, &description, start, &memory, tool_call_count, llm_call_count);

    // 8. Agent finished (or panicked).  Emit final result.
    match result {
        Ok(Ok(answer)) => {
            let _ = progress_tx.send(Progress::Done(answer));
        }
        Ok(Err(e)) => {
            let _ = progress_tx.send(Progress::Done(format!("Sub-agent error: {e}")));
        }
        Err(join_err) => {
            // The agent task panicked.
            let msg = if let Ok(reason) = join_err.try_into_panic() {
                let reason = reason
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| reason.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                format!("Sub-agent panicked: {reason}")
            } else {
                "Sub-agent task was cancelled".into()
            };
            let _ = progress_tx.send(Progress::Done(msg));
        }
    }
}

/// Helper — emit a [`TraceEvent::SubagentFinished`] if a trace store is attached.
fn emit_subagent_trace(
    trace_store: &Option<Arc<TraceStore>>,
    description: &str,
    start: Instant,
    memory: &SharedMemory,
    tool_calls: usize,
    llm_calls: usize,
) {
    if let Some(store) = trace_store {
        let duration = start.elapsed();
        let usage = memory
            .read()
            .ok()
            .and_then(|mem| {
                if mem.usage_history.is_empty() {
                    mem.last_usage.clone()
                } else {
                    let mut total = provider::Usage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    };
                    for u in &mem.usage_history {
                        total.prompt_tokens += u.prompt_tokens;
                        total.completion_tokens += u.completion_tokens;
                        total.total_tokens += u.total_tokens;
                    }
                    Some(total)
                }
            })
            .unwrap_or(provider::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            });

        store.metrics.add_subagent(description.to_string());
        store.metrics.add_token_usage(&usage);

        store.emit(TraceEvent::SubagentFinished {
            description: description.to_string(),
            steps: 0, // not tracked from parent side
            llm_calls,
            tool_calls,
            usage,
            duration,
        });
    }
}

// ── Memory builder ───────────────────────────────────────────────────────────

fn build_subagent_memory(config: &SubagentConfig, parent_memory: &SharedMemory) -> SharedMemory {
    let mut memory = Memory::new();

    // System prompt.
    memory.push(Message::new(Role::System, &config.system_prompt));

    // Optional context inheritance — copy the last N non-System messages
    // from the parent's conversation.
    if let Some(n) = config.inherit_context_messages
        && n > 0
    {
        let parent = parent_memory.read().expect("parent memory lock");
        // Collect all non-System messages, then take the last `n`.
        let all_non_system: Vec<&Message> = parent
            .messages()
            .iter()
            .filter(|m| m.role != Role::System)
            .collect();
        let start = all_non_system.len().saturating_sub(n);
        for msg in &all_non_system[start..] {
            memory.push((*msg).clone());
        }
    }

    Arc::new(std::sync::RwLock::new(memory))
}

// ── Event forwarding ─────────────────────────────────────────────────────────

/// Map a sub-agent [`engine::AgentEvent`] to a [`Progress`] event.
///
/// Non-terminal events emit `Progress::InProgress` so the parent TUI
/// shows the sub-agent's activity in real time.  Terminal events
/// (`RunCompleted`, `RunFailed`, `Cancelled`, `Done`) are NOT forwarded
/// to progress — the caller handles those to produce the final
/// `Progress::Done`.
fn forward_event_to_progress(event: engine::AgentEvent, tx: &mpsc::UnboundedSender<Progress>) {
    use engine::AgentEvent;

    match event {
        AgentEvent::Token(text) => {
            // Only forward non-empty tokens.
            if !text.is_empty() {
                let _ = tx.send(Progress::InProgress(text));
            }
        }
        AgentEvent::ReasoningToken(text) => {
            if !text.is_empty() {
                let _ = tx.send(Progress::InProgress(text));
            }
        }
        AgentEvent::ToolCallStart { name, .. } => {
            let _ = tx.send(Progress::InProgress(format!("🔧 {name}")));
        }
        AgentEvent::ToolCall {
            name, arguments, ..
        } => {
            // Truncate long arguments for readability.
            let args_summary = if arguments.len() > TRUNCATE_ARGS_CHARS {
                format!("{}…", &arguments[..TRUNCATE_ARGS_CHARS])
            } else {
                arguments
            };
            let _ = tx.send(Progress::InProgress(format!("🔧 {name}({args_summary})")));
        }
        AgentEvent::ToolProgress { name, message, .. } => {
            let _ = tx.send(Progress::InProgress(format!("  {name}: {message}")));
        }
        AgentEvent::ToolSuccessful { name, output, .. } => {
            let summary = summarize_output(&output);
            let _ = tx.send(Progress::InProgress(format!("  ✓ {name}: {summary}")));
        }
        AgentEvent::ToolFailure { name, error, .. } => {
            let _ = tx.send(Progress::InProgress(format!("  ✗ {name}: {error}")));
        }
        AgentEvent::ToolRejected { name, reason, .. } => {
            let _ = tx.send(Progress::InProgress(format!(
                "  ⊘ {name} rejected: {reason}"
            )));
        }
        // Terminal events — the caller produces the final Done from these.
        AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. } | AgentEvent::Cancelled => {
            // The caller handles these to produce final Progress::Done.
        }
        AgentEvent::Done | AgentEvent::RunStarted { .. } | AgentEvent::InterventionRequired(_) => {
            // Ignored — not forwarded to parent.
        }
    }
}

/// Produce a one-line summary of a tool result for progress display.
fn summarize_output(output: &str) -> String {
    let first_line = output.lines().next().unwrap_or("");
    let trimmed = first_line.trim();
    if trimmed.len() > OUTPUT_SUMMARY_CHARS {
        format!("{}…", &trimmed[..OUTPUT_SUMMARY_CHARS])
    } else if trimmed.is_empty() {
        "(empty output)".into()
    } else {
        trimmed.to_string()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_truncates_long_output() {
        let long = "a".repeat(200);
        let s = summarize_output(&long);
        assert!(s.ends_with('…'));
        assert!(s.len() <= OUTPUT_SUMMARY_CHARS + "…".len());
    }

    #[test]
    fn summarize_handles_empty() {
        assert_eq!(summarize_output(""), "(empty output)");
        assert_eq!(summarize_output("\nline2"), "(empty output)"); // first line is empty
    }

    #[test]
    fn summarize_preserves_short() {
        assert_eq!(summarize_output("hello world"), "hello world");
    }

    #[test]
    fn task_args_deserialize_valid() {
        let json = r#"{"description": "test", "prompt": "do something"}"#;
        let args: TaskArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.description, "test");
        assert_eq!(args.prompt, "do something");
    }

    #[test]
    fn task_args_reject_unknown_fields() {
        let json = r#"{"description": "t", "prompt": "p", "extra": 1}"#;
        let err = serde_json::from_str::<TaskArgs>(json).unwrap_err();
        assert!(err.to_string().contains("extra"));
    }
}
