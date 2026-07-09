use std::collections::HashSet;
use std::sync::Arc;

use memory::{DEFAULT_COMPACTABLE_TOOLS, DEFAULT_KEEP_RECENT_TOOL_OUTPUTS, SharedMemory};
use provider::LLMClient;
use tools::ToolRegistry;

use crate::hooks::AgentHook;

/// Configuration and dependencies for an [`Agent`](crate::Agent).
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
    // ── Tool output compaction (MicroCompact) ──────────────────────────
    /// Whether to compact old tool outputs before sending context to the LLM.
    /// When true, [`Memory::to_compact_context_vec`] is used instead of
    /// [`Memory::to_context_vec`].
    pub compact_tool_outputs: bool,
    /// Number of recent tool outputs to preserve when compacting.
    pub keep_recent_tool_outputs: usize,
    /// Tool names eligible for output compaction.
    pub compactable_tool_names: HashSet<String>,
    // ── Full LLM compaction ────────────────────────────────────────────
    /// Model to use for full-memory summarisation compaction.
    /// When `Some`, the agent checks for [`memory::CompactSignal::NeedsCompact`]
    /// after each turn and triggers an LLM summarisation pass.  Use a cheap /
    /// fast model here (e.g. `"deepseek-v4-flash"`).
    /// When `None`, only tool-output (micro) compaction runs.
    pub compact_model: Option<String>,
}

impl<C: LLMClient> EngineContext<C> {
    /// Populate the tool-output compaction fields with sensible defaults.
    ///
    /// Call this (or set the fields manually) before constructing an
    /// [`Agent`](crate::Agent) if you have `compact_tool_outputs: true`.
    pub fn with_compact_defaults(mut self) -> Self {
        self.keep_recent_tool_outputs = DEFAULT_KEEP_RECENT_TOOL_OUTPUTS;
        self.compactable_tool_names = DEFAULT_COMPACTABLE_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        self
    }
}
