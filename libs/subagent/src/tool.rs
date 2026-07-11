//! [`SubagentTool`] — a `Tool` that spawns a child [`Agent`] for complex sub-tasks.
//!
//! When the parent LLM calls the `task` tool, this implementation creates a
//! fresh agent with its own memory and a filtered tool set, runs it to
//! completion, and streams progress events back to the parent via
//! [`ProgressStream`].

use std::sync::Arc;

use engine::{Agent, EngineContext};
use memory::{Memory, SharedMemory};
use provider::{LLMClient, Message, Role};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tools::{Progress, ProgressStream, ToolError, ToolRegistry, tool};

use crate::config::SubagentConfig;

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
        }
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
    description: String,
    prompt: String,
    progress_tx: mpsc::UnboundedSender<Progress>,
) {
    // 1. Build fresh, isolated memory (user prompt is pushed automatically
    //    by `run_with_events` when the agent loop starts).
    let memory = build_subagent_memory(&config, &parent_memory);

    // 2. Build EngineContext for the sub-agent.
    let ctx = EngineContext::builder(llm, memory, subagent_tools, &config.model)
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
        .unwrap_or_else(|| std::time::Duration::from_secs(300));
    let deadline = tokio::time::Instant::now() + timeout;

    let result = loop {
        tokio::select! {
            event = sub_rx.recv() => {
                match event {
                    Some(evt) => forward_event_to_progress(evt, &progress_tx),
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
                return;
            }
        }
    };

    // 7. Agent finished (or panicked).  Emit final result.
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
        AgentEvent::ToolCall {
            name, arguments, ..
        } => {
            // Truncate long arguments for readability.
            let args_summary = if arguments.len() > 120 {
                format!("{}…", &arguments[..120])
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
        AgentEvent::Done | AgentEvent::RunStarted { .. } | AgentEvent::NeedUserIntervene(_) => {
            // Ignored — not forwarded to parent.
        }
    }
}

/// Produce a one-line summary of a tool result for progress display.
fn summarize_output(output: &str) -> String {
    let first_line = output.lines().next().unwrap_or("");
    let trimmed = first_line.trim();
    if trimmed.len() > 160 {
        format!("{}…", &trimmed[..160])
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
        assert!(s.len() <= 164); // 160 + "…"
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
