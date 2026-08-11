use std::sync::Arc;

use memory::{PendingHints, SharedMemory};
use provider::LLMClient;
use tools::ToolRegistry;

use crate::hooks::AgentHook;

/// Configuration and dependencies for an [`Agent`](crate::Agent).
///
/// All fields are public for direct construction by advanced users.
/// Most users should prefer the builder API via [`EngineContext::builder`]
/// or the even simpler [`Agent::builder`](crate::Agent::builder).
///
/// **Compaction**: both micro-compaction (tool-output clearing) and
/// macro-compaction (LLM summarisation) are provided as hooks in the
/// `hooks` crate — register [`MicroCompactHook`](hooks::MicroCompactHook)
/// and [`MacroCompactHook`](hooks::MacroCompactHook) via
/// [`hooks()`](Self::hooks).
pub struct EngineContext<C: LLMClient> {
    /// LLM provider implementation.
    pub llm: C,
    /// Shared conversation memory.
    pub memory: SharedMemory,
    /// Tool registry (shared ownership).
    pub tools: Arc<ToolRegistry>,
    /// Lifecycle hooks (optional).
    pub hooks: Vec<Box<dyn AgentHook>>,
    /// Model name to send in API requests.
    pub model: String,
    /// Safety cap — maximum loop iterations before returning an error.
    pub max_steps: usize,
    /// Maximum retry attempts for transient failures.
    pub max_retries: usize,
    /// Whether to use SSE streaming.
    pub streaming: bool,
    /// Queue for user hints injected during active runs.
    /// Drained at the start of each ReAct loop iteration.
    pub pending_hints: PendingHints,
}

impl<C: LLMClient> EngineContext<C> {
    /// Create a new [`EngineContextBuilder`] with the four **required**
    /// dependencies.
    ///
    /// All other fields use sensible defaults:
    ///
    /// | Field | Default |
    /// |-------|---------|
    /// | `hooks` | empty |
    /// | `max_steps` | `50` |
    /// | `max_retries` | `3` |
    /// | `streaming` | `true` |
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ctx = EngineContext::builder(client, memory, registry, "deepseek-v4")
    ///     .hook(my_hook)
    ///     .max_steps(100)
    ///     .build();
    /// let agent = Agent::new(ctx);
    /// ```
    pub fn builder(
        llm: C,
        memory: SharedMemory,
        tools: Arc<ToolRegistry>,
        model: impl Into<String>,
    ) -> EngineContextBuilder<C> {
        EngineContextBuilder {
            llm,
            memory,
            tools,
            model: model.into(),
            hooks: Vec::new(),
            max_steps: 50,
            max_retries: 3,
            streaming: true,
            pending_hints: PendingHints::default(),
        }
    }
}

// ── EngineContextBuilder ────────────────────────────────────────────────────

/// Fluent builder for [`EngineContext`].
///
/// Created via [`EngineContext::builder`].  Call [`build`](Self::build) to
/// produce the final [`EngineContext`].
pub struct EngineContextBuilder<C: LLMClient> {
    pub(crate) llm: C,
    pub(crate) memory: SharedMemory,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) model: String,
    pub(crate) hooks: Vec<Box<dyn AgentHook>>,
    pub(crate) max_steps: usize,
    pub(crate) max_retries: usize,
    pub(crate) streaming: bool,
    pub(crate) pending_hints: PendingHints,
}

impl<C: LLMClient> EngineContextBuilder<C> {
    /// Register a single lifecycle hook.
    pub fn hook(mut self, hook: impl AgentHook + 'static) -> Self {
        self.hooks.push(Box::new(hook));
        self
    }

    /// Register multiple lifecycle hooks at once.
    pub fn hooks(mut self, hooks: impl IntoIterator<Item = Box<dyn AgentHook>>) -> Self {
        self.hooks.extend(hooks);
        self
    }

    /// Override the default maximum loop iterations (default: `50`).
    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Override the default maximum retry attempts (default: `3`).
    pub fn max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Enable or disable SSE streaming (default: `true`).
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Provide a shared pending-hints queue for user messages injected
    /// during active agent runs (default: empty queue).
    ///
    /// Hints are drained into memory at the start of each ReAct loop
    /// iteration — after tool results from the previous step are
    /// committed, before building context for the next LLM call.
    pub fn pending_hints(mut self, pending_hints: PendingHints) -> Self {
        self.pending_hints = pending_hints;
        self
    }

    /// Consume the builder and produce an [`EngineContext`].
    pub fn build(self) -> EngineContext<C> {
        EngineContext {
            llm: self.llm,
            memory: self.memory,
            tools: self.tools,
            model: self.model,
            hooks: self.hooks,
            max_steps: self.max_steps,
            max_retries: self.max_retries,
            streaming: self.streaming,
            pending_hints: self.pending_hints,
        }
    }
}
