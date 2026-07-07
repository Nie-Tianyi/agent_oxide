//! # Agent — Core Agent Loop
//!
//! This module implements the main agent loop that drives autonomous
//! tool-using conversations with a DeepSeek LLM.
//!
//! ## Architecture
//!
//! ```text
//! run_loop()
//!   ├─ [streaming]  run_streaming_loop()
//!   └─ [non-stream] run_non_streaming_loop()
//!        │
//!        ├─ maybe_compact()          — optional memory compaction
//!        ├─ stream_with_retry()      — exponential backoff on transient errors
//!        │
//!        ├─ tool calls? → execute_all() → push results → loop
//!        └─ text?       → push to memory → return
//! ```
//!
//! ## Two response modes
//!
//! | Mode | Wire format | First-byte latency | Use case |
//! |------|------------|--------------------|----------|
//! | **Streaming** (default) | SSE (`text/event-stream`) | ~100 ms | Interactive apps, UX |
//! | **Non-streaming** | JSON (`application/json`) | Full response time | Batch, debugging |
//!
//! ## Key design decisions
//!
//! 1. **`std::sync::RwLock` + async** — Locks are scope-dropped before every
//!    `.await` point. A `std::sync` guard cannot be held across await because
//!    the future may migrate to a different OS thread when polled
//!    (tokio work-stealing). This is a fundamental Rust async pattern.
//!
//! 2. **Tool execution is synchronous** — The [`Tool`](crate::tools::Tool)
//!    trait is object-safe without `async_trait`. CPU-bound tools run inline;
//!    I/O-heavy tools can wrap their body in `tokio::task::spawn_blocking`.
//!
//! 3. **Streaming tool-call merging** — In SSE streaming, a single tool call
//!    arrives fragmented across many chunks. [`StreamAccumulator`] reassembles
//!    them by index: the first chunk carries `id` + `function.name`, subsequent
//!    chunks append to `function.arguments`.
//!
//! 4. **Batch lock acquisition** — All tool results are collected into a
//!    `Vec<Message>` first, then pushed to memory under a single write lock.
//!    This reduces contention compared to one lock per tool.
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use agent_oxide::core::agent::Agent;
//! use agent_oxide::core::client::{DeepSeekClient, Message, Role};
//! use agent_oxide::memory::Memory;
//! use agent_oxide::tools::{ToolRegistry, CalculatorTool};
//!
//! // 1. Wire up dependencies
//! let client = DeepSeekClient::new("sk-...");
//! let memory = Arc::new(std::sync::RwLock::new(Memory::new()));
//! let mut registry = ToolRegistry::new();
//! registry.register(Arc::new(CalculatorTool));
//!
//! // 2. Create the agent (streaming by default)
//! let agent = Agent::new(client, memory.clone(), Arc::new(registry))
//!     .with_model("deepseek-v4-flash")
//!     .with_max_steps(10);
//!
//! // 3. Seed the conversation
//! memory.write().unwrap().push(Message::new(Role::System, "You are helpful."));
//! memory.write().unwrap().push(Message::new(Role::User, "What is 15 * 7?"));
//!
//! // 4. Run — the agent will call the calculator tool and return the answer
//! let response = agent.run_loop().await?;
//! println!("{response}");
//! ```
//!
//! ## How streaming tool calls work
//!
//! When the model decides to call a tool, the stream delivers deltas like this:
//!
//! ```text
//! Chunk 1: delta.tool_calls = [{index: 0, id: "call_abc", function: {name: "calculator", arguments: ""}}]
//! Chunk 2: delta.tool_calls = [{index: 0, id: "",      function: {name: "",          arguments: "{\"expr"}}]
//! Chunk 3: delta.tool_calls = [{index: 0, id: "",      function: {name: "",          arguments: "ession\":"}}]
//! Chunk 4: delta.tool_calls = [{index: 0, id: "",      function: {name: "",          arguments: "\"2+2\"}"}}]
//! ```
//!
//! [`StreamAccumulator`] merges these by matching `index`, concatenating
//! `arguments` fragments until the stream signals completion (a chunk with
//! `finish_reason` set, or `data: [DONE]`).

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::core::client::{
    DeepSeekChunk, DeepSeekClient, DeepSeekError, DeepSeekRequest, DeepSeekResponse,
    DeepSeekStream, Message, Role, ToolCall, ToolCallFunction, ToolCallType, ToolChoice,
};
use crate::memory::SharedMemory;
use crate::tools::ToolRegistry;

// ── PendingConfirmations ────────────────────────────────────────────────────────

/// Shared state for pending shell-command confirmation requests.
///
/// Maps `tool_call_id` → `oneshot::Sender<bool>`. The agent inserts a sender
/// before emitting [`AgentEvent::ConfirmShell`] and then awaits the receiver.
/// The TUI thread (via [`crate::tui::event::agent_handler`]) completes the
/// sender when the user presses Y or n.
///
/// `true` = approved, `false` = denied.
///
/// # Known race
///
/// Between agent cleanup (removing the entry after timeout/response) and TUI
/// sending a response, the TUI may `send()` on an already-dropped `Sender`.
/// This is harmless — `send()` returns `Err` and the TUI ignores it — but
/// a future version could add a `log` or `tracing` warning when a stale
/// confirmation arrives.
pub type PendingConfirmations = Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>;

// ── AgentError ──────────────────────────────────────────────────────────────────

/// Errors produced by the agent loop.
///
/// Wraps the underlying LLM error type and adds agent-specific variants for
/// loop-boundary conditions.
#[derive(Debug)]
pub enum AgentError {
    /// The LLM API call failed after exhausting all retry attempts.
    DeepSeek(DeepSeekError),
    /// The loop reached [`Agent::max_steps`] without a final text response.
    MaxStepsReached(usize),
    /// The API response contained zero choices.
    NoChoices,
    /// The response had neither text content nor tool calls —
    /// the model produced an empty reply.
    NoOutput,
    /// Memory compaction failed (summariser call or internal error).
    Memory(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeepSeek(e) => write!(f, "DeepSeek error: {e}"),
            Self::MaxStepsReached(n) => {
                write!(f, "max steps ({n}) reached without a final text response")
            }
            Self::NoChoices => write!(f, "response contained no choices"),
            Self::NoOutput => write!(f, "response had neither content nor tool calls"),
            Self::Memory(msg) => write!(f, "memory error: {msg}"),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DeepSeek(e) => Some(e),
            _ => None,
        }
    }
}

// ── AgentEvent ──────────────────────────────────────────────────────────────────

/// Events emitted during [`Agent::run_with_events`] for real-time streaming UX.
///
/// The caller spawns a tokio task to consume these events from an
/// `UnboundedReceiver` and render tokens, tool calls, and results as they happen.
///
/// # Example
///
/// ```ignore
/// let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
///
/// // Spawn a render task
/// let printer = tokio::spawn(async move {
///     while let Some(event) = rx.recv().await {
///         match event {
///             AgentEvent::Token(t) => print!("{t}"),
///             AgentEvent::ToolCallStart { name, .. } => println!("\n🔧 {name}"),
///             AgentEvent::Done => println!(),
///             _ => {}
///         }
///     }
/// });
///
/// // Run the agent — events stream to the printer task
/// let response = agent.run_with_events(tx).await?;
/// printer.await.unwrap();
/// ```
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A few characters of plain-text output from the model.
    Token(String),
    /// A few characters of reasoning/chain-of-thought (DeepSeek-R1 / V4).
    ReasoningToken(String),
    /// The model has started calling a tool. `id` is the unique call identifier
    /// used to correlate subsequent fragments and the final result.
    ToolCallStart { id: String, name: String },
    /// A fragment of JSON arguments for an in-progress tool call.
    /// Concatenate these across chunks to get the full arguments string.
    ToolCallArgsDelta { id: String, delta: String },
    /// A tool call completed execution. `output` is the tool's return value.
    ToolResult {
        id: String,
        name: String,
        output: String,
    },
    /// The agent wants to execute a shell command and needs user approval.
    /// The TUI renders a confirmation prompt; the user presses Y or n.
    ConfirmShell {
        tool_call_id: String,
        command: String,
    },
    /// The agent loop completed — the model produced a final text response.
    /// The final content is returned by [`Agent::run_with_events`].
    Done,
}

// ── StreamAccumulator ──────────────────────────────────────────────────────────

/// Reassembles streaming SSE chunks into a complete response.
///
/// In streaming mode, the LLM sends many small chunks containing fragments
/// of text and/or tool calls. This struct merges those fragments into
/// coherent whole-message parts.
///
/// ## How it works
///
/// - **Content**: Simple string concatenation across chunks.
/// - **Tool calls**: Merged by chunk index (`choice.index`). The first
///   chunk for a given index carries `id` and `function.name`; subsequent
///   chunks append to `function.arguments`. We detect "first chunk" by
///   checking if the `id` field is non-empty.
///
/// After the stream ends, call [`into_parts`](Self::into_parts) to
/// extract the final `(content, tool_calls)` tuple.
#[derive(Debug, Default)]
struct StreamAccumulator {
    /// Accumulated text content from all `delta.content` fields.
    content: String,
    /// Accumulated reasoning/thinking content (DeepSeek-R1 / V4).
    reasoning: String,
    /// Tool calls under construction, keyed by chunk `index` (u32).
    /// `BTreeMap` gives deterministic iteration order sorted by index.
    tool_calls: BTreeMap<u32, PartialToolCall>,
}

/// One tool call being reassembled from streaming deltas.
#[derive(Debug, Clone, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamAccumulator {
    /// Creates an empty accumulator.
    fn new() -> Self {
        Self::default()
    }

    /// Merges one SSE chunk into the accumulator.
    ///
    /// Handles three kinds of delta data:
    /// 1. **Text content** — appended to `self.content`
    /// 2. **Reasoning content** — appended to `self.reasoning` (DeepSeek thinking)
    /// 3. **Tool call fragments** — merged into `self.tool_calls` by index
    ///
    /// For tool calls: the first chunk for a given index carries the `id`
    /// and `function.name` (detected by non-empty string). Every chunk
    /// appends its `function.arguments` to the running buffer.
    fn apply(&mut self, chunk: &DeepSeekChunk) {
        for choice in &chunk.choices {
            let delta = &choice.delta;

            // 1. Text content
            if let Some(text) = &delta.content {
                self.content.push_str(text);
            }

            // 2. Reasoning / chain-of-thought
            if let Some(reasoning) = &delta.reasoning_content {
                self.reasoning.push_str(reasoning);
            }

            // 3. Tool-call fragments
            if let Some(tool_calls) = &delta.tool_calls {
                for tc in tool_calls {
                    // Use the tool call's own index (not choice.index).
                    // Each distinct tool call in a parallel batch gets its
                    // own index (0, 1, 2, …) and fragments carry the same
                    // index across chunks.
                    let entry = self.tool_calls.entry(tc.index).or_default();

                    // First chunk for this index carries the identity fields.
                    // Later chunks have empty strings — we skip those so we
                    // don't overwrite the id/name with blanks.
                    if !tc.id.is_empty() {
                        // Detect index collision: two different call IDs
                        // assigned the same index (API protocol error).
                        if !entry.id.is_empty() && entry.id != tc.id {
                            eprintln!(
                                "WARN: StreamAccumulator index collision: \
                                 index {} had id '{}', overwriting with '{}'",
                                tc.index, entry.id, tc.id,
                            );
                        }
                        entry.id = tc.id.clone();
                    }
                    if !tc.function.name.is_empty() {
                        entry.name = tc.function.name.clone();
                    }

                    // Arguments accumulate across all chunks for this index.
                    entry.arguments.push_str(&tc.function.arguments);
                }
            }
        }
    }

    /// Consumes the accumulator, returning the final content and tool calls.
    ///
    /// Reasoning content (if any) is prepended to the text content in a
    /// marked block so the caller can distinguish thinking from output.
    fn into_parts(self) -> (String, Vec<ToolCall>) {
        let content = if self.reasoning.is_empty() {
            self.content
        } else {
            format!(
                "[Reasoning]\n{}\n[/Reasoning]\n\n{}",
                self.reasoning, self.content
            )
        };

        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .into_values()
            .map(|p| ToolCall {
                index: 0, // final assembled call — index no longer relevant
                id: p.id,
                r#type: ToolCallType::Function,
                function: ToolCallFunction {
                    name: p.name,
                    arguments: p.arguments,
                },
            })
            .collect();

        (content, tool_calls)
    }
}

// ── Agent ───────────────────────────────────────────────────────────────────────

/// A single conversational agent backed by a DeepSeek LLM.
///
/// Owns the API client, shared conversation memory, and a tool registry.
/// The main entry point is [`run_loop`](Self::run_loop), which drives the
/// conversation until the model produces a final text response or the
/// step limit is reached.
///
/// # Streaming vs non-streaming
///
/// By default the agent uses **SSE streaming** (`streaming: true`).
/// Streaming delivers tokens to the client in real time — the user sees
/// the response appear word-by-word rather than waiting for the entire
/// reply. Switch to non-streaming via [`.with_streaming(false)`](Self::with_streaming)
/// when you need the complete response at once (e.g. batch processing).
pub struct Agent {
    // ── Dependencies (injected) ──
    client: DeepSeekClient,
    memory: SharedMemory,
    registry: Arc<ToolRegistry>,

    // ── Configuration ──
    model: String,
    max_steps: usize,
    max_retries: usize,
    streaming: bool,
    compact_model: Option<String>,

    // ── Shell confirmation (optional) ──
    pending_confirmations: Option<PendingConfirmations>,
}

// ── Construction ────────────────────────────────────────────────────────────────

impl Agent {
    /// Creates a new agent with sensible defaults.
    ///
    /// # Parameters
    ///
    /// - `client` — A [`DeepSeekClient`] wired to your API key.
    /// - `memory` — Shared conversation history. The caller seeds it with
    ///   a system prompt and initial user message before calling
    ///   [`run_loop`](Self::run_loop), and can inspect it afterwards.
    /// - `registry` — Tool registry. Wrapped in `Arc` so the caller can
    ///   add tools after construction (the agent shares ownership).
    ///
    /// # Defaults
    ///
    /// | Setting | Default | Meaning |
    /// |---------|--------|---------|
    /// | `model` | `"deepseek-v4-flash"`        | Model sent in each API request |
    /// | `max_steps` | `10` | Safety cap — maximum loop iterations |
    /// | `max_retries` | `3` | Retry attempts for transient failures |
    /// | `streaming` | `true` | Use SSE streaming by default |
    /// | `compact_model` | `None` (off) | Auto-compaction disabled |
    pub fn new(client: DeepSeekClient, memory: SharedMemory, registry: Arc<ToolRegistry>) -> Self {
        Self {
            client,
            memory,
            registry,
            model: "deepseek-v4-flash".to_string(),
            max_steps: 10,
            max_retries: 3,
            streaming: true,
            compact_model: None,
            pending_confirmations: None,
        }
    }

    /// Sets the model name sent in each API request.
    ///
    /// ```ignore
    /// let agent = Agent::new(client, mem, reg)
    ///     .with_model("deepseek-v4-pro");  // use the big model
    /// ```
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Sets the maximum number of loop iterations before returning
    /// [`AgentError::MaxStepsReached`].
    ///
    /// This is a **safety guard** against infinite tool-calling loops.
    /// Each LLM call + tool-execution round counts as one step.
    pub fn with_max_steps(mut self, steps: usize) -> Self {
        self.max_steps = steps;
        self
    }

    /// Sets the number of retry attempts for transient LLM failures.
    ///
    /// **Retryable**: network errors (`DeepSeekError::Http`) and server
    /// errors (HTTP 5xx).
    ///
    /// **Not retryable**: client errors (HTTP 4xx), JSON parse failures.
    pub fn with_max_retries(mut self, retries: usize) -> Self {
        self.max_retries = retries;
        self
    }

    /// Chooses between streaming and non-streaming mode.
    ///
    /// - `true` (default) — SSE streaming, tokens appear as they're generated.
    /// - `false` — the agent waits for the full JSON response before processing.
    ///
    /// ```ignore
    /// let agent = Agent::new(client, mem, reg)
    ///     .with_streaming(false);  // batch mode
    /// ```
    pub fn with_streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Enables automatic memory compaction, using the given model for
    /// summarisation.
    ///
    /// When set, the agent checks whether the conversation exceeds the
    /// memory threshold (character count) before each LLM call and
    /// compacts if needed.
    ///
    /// Use a cheap/flash model for summarisation to minimise cost:
    ///
    /// ```ignore
    /// let agent = Agent::new(client, mem, reg)
    ///     .with_compact_model("deepseek-v4-flash");
    /// ```
    pub fn with_compact_model(mut self, model: impl Into<String>) -> Self {
        self.compact_model = Some(model.into());
        self
    }

    /// Enables user confirmation for shell commands.
    ///
    /// When set, any tool call to `"shell"` will pause the agent loop and
    /// wait for the user to approve (Y) or deny (n) via the TUI. Without
    /// this, shell commands execute unconditionally (the `--no-tui` path).
    ///
    /// ```ignore
    /// let pc = Arc::new(Mutex::new(HashMap::new()));
    /// let agent = Agent::new(client, mem, reg)
    ///     .with_pending_confirmations(pc);
    /// ```
    pub fn with_pending_confirmations(mut self, pc: PendingConfirmations) -> Self {
        self.pending_confirmations = Some(pc);
        self
    }

    /// Returns `true` if the agent is in streaming mode (SSE).
    ///
    /// When `true`, [`run_loop`](Self::run_loop) dispatches to
    /// [`run_streaming_loop`](Self::run_streaming_loop). When `false`,
    /// it uses [`run_non_streaming_loop`](Self::run_non_streaming_loop).
    pub fn streaming(&self) -> bool {
        self.streaming
    }

    /// Returns the model name used in API requests.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the maximum number of loop iterations allowed.
    pub fn max_steps(&self) -> usize {
        self.max_steps
    }

    /// Returns a reference to the pending-confirmations map, if configured.
    ///
    /// Used by [`crate::tui::event::agent_handler`] to relay user
    /// confirmation responses back to the agent.
    pub fn pending_confirmations(&self) -> Option<&PendingConfirmations> {
        self.pending_confirmations.as_ref()
    }
}

// ── Core Loop ───────────────────────────────────────────────────────────────────

impl Agent {
    /// Runs the agent's main conversation loop.
    ///
    /// Dispatches to [`run_streaming_loop`](Self::run_streaming_loop) or
    /// [`run_non_streaming_loop`](Self::run_non_streaming_loop) based on
    /// the [`streaming`](Self::with_streaming) config flag.
    ///
    /// The caller must seed [`Memory`](crate::memory::Memory) with at least
    /// a system prompt and a user message before calling this.
    ///
    /// # Returns
    ///
    /// - `Ok(content)` — the model's final text response.
    /// - `Err(AgentError::MaxStepsReached)` — the loop hit the step limit.
    /// - `Err(AgentError::DeepSeek)` — the API call failed after all retries.
    /// - `Err(AgentError::NoOutput)` — the model returned an empty response.
    ///
    /// # Real-time streaming
    ///
    /// Use [`run_with_events`](Self::run_with_events) if you need to display
    /// tokens or tool calls to the user as they happen. This method discards
    /// all intermediate events.
    pub async fn run_loop(&self) -> Result<String, AgentError> {
        if self.streaming {
            self.run_streaming_loop(None).await
        } else {
            self.run_non_streaming_loop(None).await
        }
    }

    /// Like [`run_loop`](Self::run_loop), but streams [`AgentEvent`]s through
    /// the given channel so the caller can display tokens, tool calls, and
    /// results in real time.
    ///
    /// The channel is consumed (moved into this method). When this method
    /// returns, the sender is dropped, which causes the receiver's
    /// `recv().await` to return `None` — the caller's render task can exit.
    ///
    /// # Example
    ///
    /// See the [`AgentEvent`] documentation for a complete example.
    pub async fn run_with_events(
        &self,
        tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<String, AgentError> {
        if self.streaming {
            self.run_streaming_loop(Some(tx)).await
        } else {
            self.run_non_streaming_loop(Some(tx)).await
        }
    }
}

// ── Streaming Loop ─────────────────────────────────────────────────────────────

impl Agent {
    /// Streaming agent loop — the default path.
    ///
    /// Uses SSE (Server-Sent Events) to receive model output as it's
    /// generated. [`StreamAccumulator`] reassembles the fragments.
    ///
    /// When `tx` is `Some`, [`AgentEvent`]s are sent in real time as
    /// chunks arrive — tokens, tool-call starts, arguments fragments,
    /// and tool results.
    ///
    /// # Algorithm
    ///
    /// ```text
    /// for step in 0..max_steps:
    ///     build request
    ///     open SSE stream (with retry)
    ///     for each chunk:
    ///         emit Token / ToolCallStart / ToolCallArgsDelta events
    ///         accumulate into StreamAccumulator
    ///     if tool_calls: execute → emit ToolResult events → push results → loop
    ///     else:          emit Done → push content → return
    /// ```
    async fn run_streaming_loop(
        &self,
        tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> Result<String, AgentError> {
        for _step in 0..self.max_steps {
            // ── Optional compaction ──────────────────────────────
            if let Some(ref compact_model) = self.compact_model {
                self.maybe_compact(compact_model).await?;
            }

            // ── Build request ────────────────────────────────────
            let request = self.build_request();

            // ── Stream with retry ────────────────────────────────
            let mut stream = self.stream_with_retry(request).await?;

            // ── Accumulate chunks, emitting events in real time ─
            let mut acc = StreamAccumulator::new();
            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result.map_err(AgentError::DeepSeek)?;

                // Emit events for this chunk before accumulation.
                // We clone strings here to decouple the event payloads
                // from the accumulator — events go to the channel,
                // the accumulator keeps working.
                if let Some(ref tx) = tx {
                    emit_chunk_events(tx, &chunk);
                }

                acc.apply(&chunk);
            }

            // ── Dispatch: tool calls or text? ────────────────────
            let (content, tool_calls) = acc.into_parts();

            if !tool_calls.is_empty() {
                // Model wants to use tools — execute them all.
                // If shell confirmation is configured, shell tool calls
                // will pause for user approval before executing.
                let results = if let Some(ref pc) = self.pending_confirmations {
                    self.execute_tool_calls_with_confirmations(&tool_calls, pc, tx.as_ref())
                        .await
                } else {
                    self.execute_tool_calls(&tool_calls)
                };

                // Emit ToolResult events
                if let Some(ref tx) = tx {
                    for msg in &results {
                        let tool_name = tool_calls
                            .iter()
                            .find(|tc| msg.tool_call_id.as_deref() == Some(&tc.id))
                            .map(|tc| tc.function.name.as_str())
                            .unwrap_or("unknown");
                        let _ = tx.send(AgentEvent::ToolResult {
                            id: msg.tool_call_id.clone().unwrap_or_default(),
                            name: tool_name.to_owned(),
                            output: msg.content.clone(),
                        });
                    }
                }

                {
                    let mut mem = self.memory.write().unwrap();
                    mem.push(Message::assistant_with_tools(content, tool_calls));
                    for msg in results {
                        mem.push(msg);
                    }
                }
                continue;
            }

            if content.is_empty() {
                return Err(AgentError::NoOutput);
            }

            if let Some(ref tx) = tx {
                let _ = tx.send(AgentEvent::Done);
            }

            {
                let mut mem = self.memory.write().unwrap();
                mem.push(Message::new(Role::Assistant, &content));
            }
            return Ok(content);
        }

        Err(AgentError::MaxStepsReached(self.max_steps))
    }
}

/// Walks a single SSE chunk and emits the appropriate [`AgentEvent`]s.
///
/// Each chunk may carry text content, reasoning content, and/or
/// tool-call fragments. We emit them as separate events so the
/// renderer can format each category differently.
fn emit_chunk_events(tx: &mpsc::UnboundedSender<AgentEvent>, chunk: &DeepSeekChunk) {
    for choice in &chunk.choices {
        let delta = &choice.delta;

        // Plain text
        if let Some(text) = &delta.content {
            let _ = tx.send(AgentEvent::Token(text.clone()));
        }

        // Chain-of-thought / reasoning (DeepSeek-R1 / V4)
        if let Some(reasoning) = &delta.reasoning_content {
            let _ = tx.send(AgentEvent::ReasoningToken(reasoning.clone()));
        }

        // Tool-call fragments
        if let Some(tool_calls) = &delta.tool_calls {
            for tc in tool_calls {
                // The first fragment carries the id and function name.
                if !tc.id.is_empty() && !tc.function.name.is_empty() {
                    let _ = tx.send(AgentEvent::ToolCallStart {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                    });
                }
                // Every fragment may carry arguments.
                if !tc.function.arguments.is_empty() {
                    let _ = tx.send(AgentEvent::ToolCallArgsDelta {
                        id: tc.id.clone(),
                        delta: tc.function.arguments.clone(),
                    });
                }
            }
        }
    }
}

// ── Non-Streaming Loop ─────────────────────────────────────────────────────────

impl Agent {
    /// Non-streaming agent loop — the classic path.
    ///
    /// Sends a single HTTP request and receives the complete JSON response.
    /// Simpler than streaming but has higher time-to-first-token latency.
    ///
    /// When `tx` is `Some`, emits [`AgentEvent::ToolCallStart`],
    /// [`AgentEvent::ToolCallArgsDelta`], and [`AgentEvent::ToolResult`]
    /// for tool calls (but not [`AgentEvent::Token`] — there are no
    /// streaming chunks in this mode).
    ///
    /// # Algorithm
    ///
    /// ```text
    /// for step in 0..max_steps:
    ///     build request
    ///     send() with retry → response
    ///     if response has tool_calls: emit events → execute → push results → loop
    ///     else:                       emit Done → push content → return
    /// ```
    async fn run_non_streaming_loop(
        &self,
        tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> Result<String, AgentError> {
        for _step in 0..self.max_steps {
            // ── Optional compaction ──────────────────────────────
            if let Some(ref compact_model) = self.compact_model {
                self.maybe_compact(compact_model).await?;
            }

            // ── Build request ────────────────────────────────────
            let request = self.build_request();

            // ── Call LLM ─────────────────────────────────────────
            let response = self.send_with_retry(request).await?;

            let choice = response.choices.first().ok_or(AgentError::NoChoices)?;

            // ── Tool calls? ──────────────────────────────────────
            if let Some(tool_calls) = &choice.message.tool_calls
                && !tool_calls.is_empty()
            {
                let tool_calls = tool_calls.clone();
                let content = choice.message.content.clone().unwrap_or_default();

                // Emit tool-call events (non-streaming: all info in one go)
                if let Some(ref tx) = tx {
                    for tc in &tool_calls {
                        let _ = tx.send(AgentEvent::ToolCallStart {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                        });
                        let _ = tx.send(AgentEvent::ToolCallArgsDelta {
                            id: tc.id.clone(),
                            delta: tc.function.arguments.clone(),
                        });
                    }
                }

                // Execute tools — with shell confirmation if configured.
                let results = if let Some(ref pc) = self.pending_confirmations {
                    self.execute_tool_calls_with_confirmations(&tool_calls, pc, tx.as_ref())
                        .await
                } else {
                    self.execute_tool_calls(&tool_calls)
                };

                // Emit ToolResult events
                if let Some(ref tx) = tx {
                    for msg in &results {
                        let tool_name = tool_calls
                            .iter()
                            .find(|tc| msg.tool_call_id.as_deref() == Some(&tc.id))
                            .map(|tc| tc.function.name.as_str())
                            .unwrap_or("unknown");
                        let _ = tx.send(AgentEvent::ToolResult {
                            id: msg.tool_call_id.clone().unwrap_or_default(),
                            name: tool_name.to_owned(),
                            output: msg.content.clone(),
                        });
                    }
                }

                // One write lock for both the assistant message and all tool results
                {
                    let mut mem = self.memory.write().unwrap();
                    mem.push(Message::assistant_with_tools(content, tool_calls));
                    for msg in results {
                        mem.push(msg);
                    }
                }
                continue;
            }

            // ── Text response — done ─────────────────────────────
            let content = choice.message.content.clone().unwrap_or_default();
            if let Some(ref tx) = tx {
                let _ = tx.send(AgentEvent::Done);
            }
            {
                let mut mem = self.memory.write().unwrap();
                mem.push(Message::new(Role::Assistant, &content));
            }
            return Ok(content);
        }

        Err(AgentError::MaxStepsReached(self.max_steps))
    }
}

// ── Shared Helpers ─────────────────────────────────────────────────────────────

impl Agent {
    /// Builds a [`DeepSeekRequest`] from the current memory and tool registry.
    ///
    /// Reads messages under a read lock (cloned, lock dropped before return).
    /// Attaches tool definitions if the registry is non-empty, with
    /// `tool_choice: auto` so the model can decide whether to call tools.
    fn build_request(&self) -> DeepSeekRequest {
        let messages = {
            let mem = self.memory.read().unwrap();
            mem.to_context_vec()
        };

        let mut request = DeepSeekRequest::new(&self.model, messages);

        let tool_defs = self.registry.to_tool_defs();
        if !tool_defs.is_empty() {
            request.tools = Some(tool_defs);
            request.tool_choice = Some(ToolChoice::Auto);
        }

        request
    }

    /// Executes a batch of tool calls and wraps each result in a
    /// `Role::Tool` message.
    ///
    /// Returns a `Vec<Message>` ready to push into memory. The caller
    /// controls lock acquisition — this method holds no locks.
    fn execute_tool_calls(&self, tool_calls: &[ToolCall]) -> Vec<Message> {
        tool_calls
            .iter()
            .map(|tc| {
                let output = match self
                    .registry
                    .execute(&tc.function.name, &tc.function.arguments)
                {
                    Some(Ok(result)) => result,
                    Some(Err(e)) => format!("Error: {e}"),
                    None => format!("Tool '{}' not found", tc.function.name),
                };
                Message::tool_result(&tc.id, output)
            })
            .collect()
    }

    /// Executes tool calls with optional user confirmation for shell commands.
    ///
    /// When `pending` is provided (TUI mode), any tool call named `"shell"`
    /// is intercepted: the agent sends a [`AgentEvent::ConfirmShell`] to the
    /// TUI, then awaits the user's response via a oneshot channel. Non-shell
    /// tools execute immediately as usual.
    ///
    /// When `pending` is `None` (CLI mode), all tools execute unconditionally.
    async fn execute_tool_calls_with_confirmations(
        &self,
        tool_calls: &[ToolCall],
        pending: &PendingConfirmations,
        tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    ) -> Vec<Message> {
        let mut results = Vec::with_capacity(tool_calls.len());

        for tc in tool_calls {
            if tc.function.name == "shell" {
                // Extract the command arg for display to the user.
                let command = Self::extract_shell_command_for_display(&tc.function.arguments);

                // ── Set up oneshot channel ─────────────────────
                let (confirm_tx, confirm_rx) = oneshot::channel();
                {
                    let mut map = pending.lock().unwrap();
                    map.insert(tc.id.clone(), confirm_tx);
                }

                // ── Notify TUI ────────────────────────────────
                if let Some(tx) = tx {
                    let _ = tx.send(AgentEvent::ConfirmShell {
                        tool_call_id: tc.id.clone(),
                        command: command.clone(),
                    });
                }

                // ── Wait for user response ────────────────────
                // 5-minute safety timeout prevents a hung TUI from
                // blocking the agent indefinitely.
                let approved = tokio::time::timeout(Duration::from_secs(300), confirm_rx)
                    .await
                    .map(|r| r.unwrap_or(false))
                    .unwrap_or(false);

                // ── Cleanup ──────────────────────────────────
                {
                    let mut map = pending.lock().unwrap();
                    map.remove(&tc.id);
                }

                // ── Execute or deny ──────────────────────────
                if approved {
                    let output = self
                        .registry
                        .execute(&tc.function.name, &tc.function.arguments);
                    let result = match output {
                        Some(Ok(s)) => s,
                        Some(Err(e)) => format!("Error: {e}"),
                        None => format!("Tool '{}' not found", tc.function.name),
                    };
                    results.push(Message::tool_result(&tc.id, result));
                } else {
                    results.push(Message::tool_result(
                        &tc.id,
                        "User denied the command. Inform the user that the \
                         command was not executed.",
                    ));
                }
            } else {
                // Non-shell tool — execute unconditionally.
                let output = self
                    .registry
                    .execute(&tc.function.name, &tc.function.arguments);
                let result = match output {
                    Some(Ok(s)) => s,
                    Some(Err(e)) => format!("Error: {e}"),
                    None => format!("Tool '{}' not found", tc.function.name),
                };
                results.push(Message::tool_result(&tc.id, result));
            }
        }

        results
    }

    /// Extracts the `command` field from a JSON arguments string for
    /// display in the confirmation prompt. Returns the raw args on
    /// parse failure so the user can still see what was requested.
    fn extract_shell_command_for_display(args: &str) -> String {
        serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| v.get("command").and_then(|c| c.as_str().map(String::from)))
            .unwrap_or_else(|| args.to_string())
    }
}

// ── Retry Logic ────────────────────────────────────────────────────────────────

impl Agent {
    /// Sends a non-streaming request with exponential-backoff retry.
    ///
    /// Backoff schedule: 500ms → 1s → 2s → 4s → …
    /// Only network errors and HTTP 5xx are retried; 4xx and parse errors
    /// fail immediately.
    async fn send_with_retry(
        &self,
        request: DeepSeekRequest,
    ) -> Result<DeepSeekResponse, AgentError> {
        let mut last_err: Option<DeepSeekError> = None;

        for attempt in 0..self.max_retries {
            match self.client.send(request.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    if !is_retryable(&e) {
                        return Err(AgentError::DeepSeek(e));
                    }
                    last_err = Some(e);
                    if attempt + 1 < self.max_retries {
                        let delay = Duration::from_millis(500 * (1 << attempt));
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        // If max_retries is 0 and the loop body never executed, last_err is None.
        // Use a fallback error to avoid panicking.
        Err(AgentError::DeepSeek(last_err.unwrap_or_else(|| {
            DeepSeekError::Parse("max_retries is 0; no attempts were made".into())
        })))
    }

    /// Opens a streaming SSE connection with exponential-backoff retry.
    ///
    /// Retry logic mirrors [`send_with_retry`](Self::send_with_retry):
    /// only the initial HTTP handshake is retried. Once the stream starts,
    /// individual chunk errors are fatal (no mid-stream retry).
    async fn stream_with_retry(
        &self,
        request: DeepSeekRequest,
    ) -> Result<DeepSeekStream, AgentError> {
        let mut last_err: Option<DeepSeekError> = None;

        for attempt in 0..self.max_retries {
            match self.client.stream(request.clone()).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    if !is_retryable(&e) {
                        return Err(AgentError::DeepSeek(e));
                    }
                    last_err = Some(e);
                    if attempt + 1 < self.max_retries {
                        let delay = Duration::from_millis(500 * (1 << attempt));
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(AgentError::DeepSeek(last_err.unwrap_or_else(|| {
            DeepSeekError::Parse("max_retries is 0; no attempts were made".into())
        })))
    }
}

// ── Compaction ─────────────────────────────────────────────────────────────────

impl Agent {
    /// Checks whether memory needs compaction and, if so, performs the
    /// two-phase drain → summarise → apply cycle.
    ///
    /// Locks are taken and dropped around each phase to avoid holding a
    /// `std::sync::RwLock` guard across `.await` points.
    ///
    /// # Phases
    ///
    /// 1. **Drain** (write lock) — remove old non-System messages from memory.
    /// 2. **Summarise** (no lock) — call the compact model to produce a summary.
    /// 3. **Apply** (write lock) — insert the summary as a new System message.
    async fn maybe_compact(&self, compact_model: &str) -> Result<(), AgentError> {
        // Check threshold under read lock
        let needs = {
            let mem = self.memory.read().unwrap();
            mem.needs_compact()
        };

        if !needs {
            return Ok(());
        }

        // Phase 1: drain under write lock — re-check threshold inside
        // the lock to avoid TOCTOU: new messages may have been pushed
        // between the read-lock check above and this write-lock acquisition.
        let drained = {
            let mut mem = self.memory.write().unwrap();
            if !mem.needs_compact() {
                return Ok(());
            }
            mem.drain_for_compact()
        };

        if drained.is_empty() {
            return Ok(());
        }

        // Build summary transcript (no lock held)
        let transcript: String = drained
            .iter()
            .map(|m| format!("[{}]: {}", role_label(m.role), m.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "Summarise the following conversation history concisely. \
             Preserve key facts, decisions, and context. \
             Output only the summary, no preamble:\n\n{transcript}"
        );

        // Phase 2: call LLM (no lock held)
        let summary_req =
            DeepSeekRequest::new(compact_model, vec![Message::new(Role::User, prompt)]);

        let summary = self
            .client
            .send(summary_req)
            .await
            .map_err(|e| AgentError::Memory(format!("compaction LLM call failed: {e}")))?
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        // Phase 3: apply under write lock
        {
            let mut mem = self.memory.write().unwrap();
            mem.apply_compact(summary);
        }

        Ok(())
    }
}

// ── Free Functions ─────────────────────────────────────────────────────────────

/// Returns `true` if the error is transient and worth retrying.
///
/// | Category | Retry? | Rationale |
/// |----------|--------|-----------|
/// | `Http` (network) | ✅ yes | DNS, connection refused, timeout — likely temporary |
/// | `Api { 5xx }` | ✅ yes | Server overload, gateway timeout — transient |
/// | `Api { 4xx }` | ❌ no | Bad request, unauthorised — retrying won't fix it |
/// | `Parse` | ❌ no | Malformed response — retrying won't change the bytes |
/// | `StreamingNotSupported` | ❌ no | Configuration error — retrying pointless |
fn is_retryable(err: &DeepSeekError) -> bool {
    match err {
        DeepSeekError::Http(_) => true,
        DeepSeekError::Api { status, .. } => *status >= 500,
        DeepSeekError::Parse(_) | DeepSeekError::StreamingNotSupported => false,
    }
}

/// Human-readable label for each [`Role`], used when formatting
/// conversation transcripts for summarisation.
///
/// Note: a duplicate of this function exists in `memory::role_label`.
/// Both are `pub(crate)`-invisible to each other; keeping a local copy
/// avoids coupling the agent to a memory-internal helper.
const fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::memory::Memory;
    use crate::tools::EchoTool;

    // ── Test helpers ────────────────────────────────────────────────

    /// Builds a bare-minimum agent for construction/config tests.
    /// Uses a fake API key — these tests never hit the network.
    fn make_agent() -> Agent {
        let client = DeepSeekClient::new("test-api-key");
        let memory = Arc::new(std::sync::RwLock::new(Memory::new()));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        Agent::new(client, memory, Arc::new(registry))
    }

    /// Creates a minimal valid DeepSeekChunk for accumulator tests.
    fn make_chunk(content: Option<&str>, tool_calls: Option<Vec<ToolCall>>) -> DeepSeekChunk {
        use crate::core::client::{ChunkChoice, Delta};
        DeepSeekChunk {
            id: "test-chunk".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "deepseek-v4-flash".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content: content.map(String::from),
                    reasoning_content: None,
                    tool_calls,
                },
                finish_reason: None,
            }],
            usage: None,
        }
    }

    fn make_tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            index: 0,
            id: id.into(),
            r#type: ToolCallType::Function,
            function: ToolCallFunction {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }

    // ── Construction ──────────────────────────────────────────────

    #[test]
    fn test_agent_new_defaults() {
        let agent = make_agent();
        assert_eq!(agent.model, "deepseek-v4-flash");
        assert_eq!(agent.max_steps, 10);
        assert_eq!(agent.max_retries, 3);
        assert!(agent.streaming, "streaming should be on by default");
        assert!(agent.compact_model.is_none());
    }

    #[test]
    fn test_agent_builder() {
        let client = DeepSeekClient::new("test-key");
        let memory = Arc::new(std::sync::RwLock::new(Memory::new()));
        let registry = Arc::new(ToolRegistry::new());

        let agent = Agent::new(client, memory, registry)
            .with_model("deepseek-v4-pro")
            .with_max_steps(5)
            .with_max_retries(2)
            .with_streaming(false)
            .with_compact_model("deepseek-v4-flash");

        assert_eq!(agent.model, "deepseek-v4-pro");
        assert_eq!(agent.max_steps, 5);
        assert_eq!(agent.max_retries, 2);
        assert!(!agent.streaming);
        assert_eq!(agent.compact_model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn test_agent_builder_default_model() {
        let agent = make_agent();
        assert_eq!(agent.model, "deepseek-v4-flash");
    }

    #[test]
    fn test_agent_memory_is_shared() {
        let agent = make_agent();

        {
            let mut mem = agent.memory.write().unwrap();
            mem.push(Message::new(Role::User, "hello"));
        }

        {
            let mem = agent.memory.read().unwrap();
            assert_eq!(mem.message_count(), 1);
            assert_eq!(mem.messages()[0].content, "hello");
        }
    }

    // ── StreamAccumulator ─────────────────────────────────────────

    #[test]
    fn test_accumulator_content_only() {
        let mut acc = StreamAccumulator::new();

        acc.apply(&make_chunk(Some("Hello"), None));
        acc.apply(&make_chunk(Some(" world"), None));
        acc.apply(&make_chunk(Some("!"), None));

        let (content, tool_calls) = acc.into_parts();
        assert_eq!(content, "Hello world!");
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_accumulator_empty_returns_empty() {
        let acc = StreamAccumulator::new();
        let (content, tool_calls) = acc.into_parts();
        assert!(content.is_empty());
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_accumulator_tool_call_single_chunk() {
        let mut acc = StreamAccumulator::new();

        // Sometimes the full tool call arrives in one chunk (when finish_reason
        // is present in the same chunk)
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("call_1", "echo", r#"{"text":"hi"}"#)]),
        ));

        let (content, tool_calls) = acc.into_parts();
        assert!(content.is_empty());
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].function.name, "echo");
        assert_eq!(tool_calls[0].function.arguments, r#"{"text":"hi"}"#);
    }

    #[test]
    fn test_accumulator_tool_call_fragmented() {
        let mut acc = StreamAccumulator::new();

        // Simulate streaming: id + name in first chunk, arguments in later chunks
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("call_abc", "calculator", "")]),
        ));
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("", "", r#"{"expr"#)]),
        ));
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("", "", "ession\":")]),
        ));
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("", "", "\"2+2\"}")]),
        ));

        let (content, tool_calls) = acc.into_parts();
        assert!(content.is_empty());
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_abc");
        assert_eq!(tool_calls[0].function.name, "calculator");
        assert_eq!(tool_calls[0].function.arguments, r#"{"expression":"2+2"}"#);
    }

    #[test]
    fn test_accumulator_content_and_tool_calls() {
        let mut acc = StreamAccumulator::new();

        // Model might emit text before calling a tool
        acc.apply(&make_chunk(Some("Let me calculate that."), None));
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("call_x", "calc", "{}")]),
        ));

        let (content, tool_calls) = acc.into_parts();
        assert_eq!(content, "Let me calculate that.");
        assert_eq!(tool_calls.len(), 1);
    }

    #[test]
    fn test_accumulator_multiple_tool_calls() {
        let mut acc = StreamAccumulator::new();

        // Two tool calls arriving interleaved — typical for multi-tool responses.
        // In practice each tool call has a different chunk index, but both can
        // share the same index if they're in sequential chunks. We simulate
        // by using index 0 for the first call's first chunk, etc.
        //
        // Tool call 0: full in one chunk
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("id_a", "echo", r#"{"text":"a"}"#)]),
        ));
        // Tool call 1: fragmented across two chunks
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("id_b", "calc", r#"{"expr""#)]),
        ));
        acc.apply(&make_chunk(
            None,
            Some(vec![make_tool_call("", "", "ession\":\"1+1\"}")]),
        ));

        let (_content, tool_calls) = acc.into_parts();

        // Note: BTreeMap merges by chunk index. Both calls use index 0,
        // so the second overwrites/extends the first. This is expected
        // behavior — in real streaming, each distinct tool call gets its
        // own index (0, 1, 2, …).
        assert_eq!(tool_calls.len(), 1, "same index → merged into one call");
    }

    // ── Error Display ─────────────────────────────────────────────

    #[test]
    fn test_agent_error_display_max_steps() {
        let e = AgentError::MaxStepsReached(10);
        let s = e.to_string();
        assert!(s.contains("max steps"), "got: {s}");
        assert!(s.contains("10"), "got: {s}");
    }

    #[test]
    fn test_agent_error_display_no_choices() {
        let s = AgentError::NoChoices.to_string();
        assert!(s.contains("no choices"), "got: {s}");
    }

    #[test]
    fn test_agent_error_display_no_output() {
        let s = AgentError::NoOutput.to_string();
        assert!(s.contains("neither content nor tool"), "got: {s}");
    }

    #[test]
    fn test_agent_error_display_deepseek() {
        let inner = DeepSeekError::Parse("bad json".into());
        let e = AgentError::DeepSeek(inner);
        let s = e.to_string();
        assert!(s.contains("DeepSeek error"), "got: {s}");
        assert!(s.contains("parse error"), "got: {s}");
    }

    #[test]
    fn test_agent_error_display_memory() {
        let e = AgentError::Memory("oom".into());
        let s = e.to_string();
        assert!(s.contains("memory error"), "got: {s}");
        assert!(s.contains("oom"), "got: {s}");
    }

    #[test]
    fn test_agent_error_source() {
        let inner = DeepSeekError::Parse("x".into());
        let e = AgentError::DeepSeek(inner);
        assert!(e.source().is_some());

        assert!(AgentError::MaxStepsReached(1).source().is_none());
        assert!(AgentError::NoChoices.source().is_none());
        assert!(AgentError::NoOutput.source().is_none());
        assert!(AgentError::Memory("x".into()).source().is_none());
    }

    // ── is_retryable ──────────────────────────────────────────────

    #[test]
    fn test_is_retryable_server_errors() {
        assert!(is_retryable(&DeepSeekError::Api {
            status: 500,
            body: "boom".into()
        }));
        assert!(is_retryable(&DeepSeekError::Api {
            status: 502,
            body: "boom".into()
        }));
        assert!(is_retryable(&DeepSeekError::Api {
            status: 503,
            body: "boom".into()
        }));
    }

    #[test]
    fn test_is_retryable_client_errors() {
        for status in [400, 401, 404, 429] {
            assert!(
                !is_retryable(&DeepSeekError::Api {
                    status,
                    body: "".into()
                }),
                "status {status} should NOT be retryable"
            );
        }
    }

    #[test]
    fn test_is_retryable_parse_not_retryable() {
        assert!(!is_retryable(&DeepSeekError::Parse("bad".into())));
    }

    #[test]
    fn test_is_retryable_streaming_not_supported() {
        assert!(!is_retryable(&DeepSeekError::StreamingNotSupported));
    }

    // ── role_label ────────────────────────────────────────────────

    #[test]
    fn test_role_label_all_variants() {
        assert_eq!(role_label(Role::System), "System");
        assert_eq!(role_label(Role::User), "User");
        assert_eq!(role_label(Role::Assistant), "Assistant");
        assert_eq!(role_label(Role::Tool), "Tool");
    }

    // ── Request building (unit-level) ─────────────────────────────

    #[test]
    fn test_build_request_includes_messages() {
        let agent = make_agent();
        {
            let mut mem = agent.memory.write().unwrap();
            mem.push(Message::new(Role::System, "Be helpful."));
            mem.push(Message::new(Role::User, "Hi"));
        }

        let request = agent.build_request();
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.model, "deepseek-v4-flash");
        assert!(!request.stream, "build_request should not set stream");
    }

    #[test]
    fn test_build_request_includes_tools() {
        let agent = make_agent(); // EchoTool registered
        {
            let mut mem = agent.memory.write().unwrap();
            mem.push(Message::new(Role::User, "test"));
        }

        let request = agent.build_request();
        assert!(request.tools.is_some());
        let tools = request.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "echo");
    }

    #[test]
    fn test_build_request_no_tools_when_registry_empty() {
        let client = DeepSeekClient::new("key");
        let memory = Arc::new(std::sync::RwLock::new(Memory::new()));
        let registry = Arc::new(ToolRegistry::new()); // empty
        let agent = Agent::new(client, memory, registry);
        {
            let mut mem = agent.memory.write().unwrap();
            mem.push(Message::new(Role::User, "test"));
        }

        let request = agent.build_request();
        assert!(request.tools.is_none());
    }
}
