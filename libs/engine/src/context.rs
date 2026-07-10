use std::sync::Arc;

use memory::SharedMemory;
use provider::LLMClient;
use tools::ToolRegistry;

use crate::hooks::AgentHook;

/// Configuration for macro-compaction (full LLM summarisation).
///
/// When `Some`, the agent checks whether the conversation exceeds
/// `threshold` characters before each LLM call and, if so, drains
/// old non-System messages and summarises them via the given `model`.
#[derive(Debug, Clone)]
pub struct MacroCompactConfig {
    /// Model name for summarisation (cheap / fast model).
    pub model: String,
    /// Character budget that triggers summarisation.
    pub threshold: usize,
    /// Number of non-System messages preserved during drain.
    pub keep_last_n: usize,
}

/// Configuration and dependencies for an [`Agent`](crate::Agent).
///
/// All fields are public for direct construction by advanced users.
/// Most users should prefer the builder API via [`EngineContext::builder`]
/// or the even simpler [`Agent::builder`](crate::Agent::builder).
pub struct EngineContext<C: LLMClient> {
    /// LLM provider implementation.
    pub llm: C,
    /// Shared conversation memory.
    pub memory: SharedMemory,
    /// Tool registry (shared ownership).
    pub tools: Arc<ToolRegistry>,
    /// Lifecycle hooks (optional).  Compaction, sandbox approval, and
    /// other policies are provided by hooks in the `hooks` crate.
    pub hooks: Vec<Box<dyn AgentHook>>,
    /// Model name to send in API requests.
    pub model: String,
    /// Safety cap — maximum loop iterations before returning an error.
    pub max_steps: usize,
    /// Maximum retry attempts for transient failures.
    pub max_retries: usize,
    /// Whether to use SSE streaming.
    pub streaming: bool,
    /// Macro-compaction configuration.  When `Some`, the agent runs
    /// LLM summarisation when the character budget is exceeded.
    /// Micro-compaction (tool-output clearing) is handled by
    /// [`MicroCompactHook`](hooks::MicroCompactHook) registered as an
    /// [`AgentHook`](crate::AgentHook).
    pub macro_compact: Option<MacroCompactConfig>,
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
            macro_compact: None,
        }
    }
}

// ── EngineContextBuilder ────────────────────────────────────────────────────

/// Fluent builder for [`EngineContext`].
///
/// Created via [`EngineContext::builder`].  Call [`build`](Self::build) to
/// produce the final [`EngineContext`].
///
/// This is the **advanced** API — most users should prefer
/// [`Agent::builder`](crate::Agent::builder) which wraps this builder with
/// convenient defaults (auto-created memory, tool registration, system
/// prompt seeding).
pub struct EngineContextBuilder<C: LLMClient> {
    pub(crate) llm: C,
    pub(crate) memory: SharedMemory,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) model: String,
    pub(crate) hooks: Vec<Box<dyn AgentHook>>,
    pub(crate) max_steps: usize,
    pub(crate) max_retries: usize,
    pub(crate) streaming: bool,
    pub(crate) macro_compact: Option<MacroCompactConfig>,
}

impl<C: LLMClient> EngineContextBuilder<C> {
    /// Register a single lifecycle hook.
    ///
    /// Hooks are called in the order they are registered.
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
    ///
    /// When the agent reaches this many ReAct loop steps it returns
    /// [`AgentError::MaxStepsReached`](crate::AgentError::MaxStepsReached).
    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Override the default maximum retry attempts (default: `3`).
    ///
    /// Transient LLM provider failures are retried with exponential
    /// backoff up to this many times.
    pub fn max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Enable or disable SSE streaming (default: `true`).
    ///
    /// When enabled the agent uses `LLMClient::stream()` and emits
    /// [`AgentEvent::Token`] and [`AgentEvent::ToolCallArgsDelta`] events
    /// in real time.  When disabled it uses `LLMClient::generate()` and
    /// emits a single [`AgentEvent::Token`] with the full response.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Enable macro-compaction (full LLM summarisation).
    ///
    /// When set, the agent checks the character budget before each
    /// LLM call and summarises old messages via the given model.
    pub fn macro_compact(mut self, config: MacroCompactConfig) -> Self {
        self.macro_compact = Some(config);
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
            macro_compact: self.macro_compact,
        }
    }
}
