//! Configuration for [`SubagentTool`].

/// Configuration for a subagent invocation.
///
/// All fields have sensible defaults.  The only required field is
/// [`model`](Self::model), which must be set explicitly by the caller.
#[derive(Clone, Debug)]
pub struct SubagentConfig {
    /// LLM model name for the subagent (e.g. `"deepseek-v4-flash"`).
    pub model: String,

    /// System prompt injected into the subagent's fresh memory.
    pub system_prompt: String,

    /// Maximum ReAct loop iterations before the subagent is terminated.
    pub max_steps: usize,

    /// Maximum retries for transient LLM provider failures.
    pub max_retries: usize,

    /// Whether to enable SSE streaming for the subagent's LLM calls.
    pub streaming: bool,

    /// Hard wall-clock timeout in seconds for the entire subagent run.
    /// When elapsed, the subagent task is aborted and a timeout message
    /// is returned as the tool result.  `None` disables the timeout.
    pub timeout_secs: Option<u64>,

    /// If `Some(n)`, copy the last `n` non-System messages from the
    /// parent's conversation memory into the subagent's fresh memory
    /// as inherited context.  Useful for maintaining continuity across
    /// delegation calls.  `None` means no context is inherited.
    pub inherit_context_messages: Option<usize>,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            model: String::new(), // must be set explicitly
            system_prompt: "\
You are a focused workspace sub-agent with access to file-system tools.
Your job is to complete the assigned task carefully and accurately.
You have read-only access: you can read files, list directories, glob
for files, grep for content, and use a calculator. You CANNOT write,
edit, or execute shell commands — use the tools you have to investigate
and report your findings concisely.
"
            .into(),
            max_steps: 25,
            max_retries: 2,
            streaming: true,
            timeout_secs: Some(120),
            inherit_context_messages: None,
        }
    }
}
