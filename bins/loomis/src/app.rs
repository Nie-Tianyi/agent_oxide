//! Agent assembly — wires all components together.

use std::path::Path;
use std::sync::Arc;

use deepseek::DeepSeekClient;
use engine::{Agent, EngineContext};
use hooks;
use memory::{Memory, SharedMemory};
use provider::{Message, Role};
use tokio::sync::mpsc;
use tools::ToolRegistry;

use tools::SandboxConfig;

use crate::hooks::SandboxHook;
use crate::sandbox::audit_logger::AuditLogger;
use crate::sandbox::resource_tracker::ResourceTracker;
use crate::sandbox::shell_filter::ShellFilter;
use crate::tools::{
    CalculatorTool, EditTool, GlobTool, GrepTool, LsTool, ReadTool, ShellTool, WriteTool,
};

/// System prompt used as the initial seed for every conversation.
pub const SYSTEM_PROMPT: &str = "\
You are Loomis, a helpful, accurate coding assistant. You have tools for file operations \
(read, write, edit, glob, grep, ls) and calculations.

## Core rules — follow strictly

1. **Ground everything in tools.** Before making ANY claim about file paths, \
code contents, directory structure, or the codebase: verify with the \
appropriate tool (glob to find files, grep to search content, read to read, \
ls to list). Never guess. If a tool returns nothing or errors, report that \
honestly — do not fabricate a result.

2. **Express uncertainty.** If you don't know something or can't verify it, \
say so. It is better to admit uncertainty than to give a confident wrong \
answer. If the user \
asks something ambiguous, ask for clarification.

3. **Quote, don't summarise from memory.** When referencing code, always read \
the file first and quote the actual content. Never invent function signatures, \
variable names, or line numbers.

4. **Verify before editing.** Before writing or editing a file, read it first. \
Before running a glob, check the directory exists. Before claiming a fix works, \
explain what you verified.

5. **No phantom files or features.** If the user mentions a file that doesn't \
exist, say so. If they ask you to implement something, only write code that \
actually compiles and uses real APIs.

6. **Use the right tool for the job.** grep to search content, glob to find \
files by name, ls to list directories, read to view contents, write to create, \
edit to modify. Don't try to use read where grep is appropriate. Only use shell \
when necessary.

7. **Be concise and accurate.** Short, factual responses are better than long, \
speculative ones. Respond in the same language the user uses.

8. **Readability over Performance**: Code readability takes precedence over \
performance. User expect code of pedagogical quality: make clear the purpose \
of every variable name and every struct. If there are two algorithms A and B, \
where A is easier to understand but B is harder yet offers better performance, \
always prefer algorithm A unless B is significantly faster than A (at least three \
times as fast). When algorithm B is chosen, it must be accompanied by thorough \
documentation, including but not limited to its purpose, inputs, outputs, underlying \
principles, etc. When necessary, educate your users—do not assume they have any \
background of the field.
";

// ── AgentEvent (re-exported from engine) ──────────────────────────────────────

/// Re-export the engine's event type for channel construction.
pub use engine::AgentEvent;

// ── HookEvent ─────────────────────────────────────────────────────────────────

/// Events produced by loomis-side components (SandboxHook, agent_handler)
/// that are **not** part of the engine's generic [`AgentEvent`] enum.
///
/// These are shell-specific events: running a user `!command`, its output,
/// and approval prompts for dangerous shell operations.
///
/// Sent over a separate `mpsc::unbounded_channel` — the TUI event loop
/// polls both the agent channel and the hook channel every frame.
#[derive(Debug, Clone)]
pub enum HookEvent {
    /// The user's `!command` shell invocation has started executing.
    ShellRunning { command: String },
    /// A user `!command` has completed with its captured stdout/stderr.
    ShellOutput { command: String, output: String },
    /// The [`SandboxHook`] is requesting user approval before executing a
    /// shell command on the agent's behalf.
    ShellApprovalRequested {
        tool_call_id: String,
        command: String,
    },
}

/// Product of [`build_coding_agent`] — everything needed to launch the TUI.
pub struct AgentKit {
    pub agent: Agent<DeepSeekClient>,
    pub memory: SharedMemory,
    pub tool_names: Vec<String>,
    pub model: String,
    /// Receiving half of the agent-event channel — consumed by the TUI event loop.
    pub agent_rx: mpsc::UnboundedReceiver<AgentEvent>,
    /// Clone of the sending half — for the agent handler background task.
    pub agent_tx: mpsc::UnboundedSender<AgentEvent>,
    /// Receiving half of the hook-event channel — for shell events.
    pub hook_rx: mpsc::UnboundedReceiver<HookEvent>,
    /// Clone of the sending half — for [`SandboxHook`] and agent_handler shell commands.
    pub hook_tx: mpsc::UnboundedSender<HookEvent>,
    /// The sender that unblocks the approval hook when the user
    /// answers a shell confirmation prompt.
    pub approval_tx: std::sync::mpsc::SyncSender<bool>,
}

/// Build a fully-wired coding agent with all channels and hooks.
pub fn build_coding_agent(
    api_key: &str,
    workspace_root: &Path,
    model: &str,
    flash_model: &str,
    sandbox_config: &SandboxConfig,
) -> AgentKit {
    // ── Channels ──────────────────────────────────────────────
    let (agent_tx, agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (hook_tx, hook_rx) = mpsc::unbounded_channel::<HookEvent>();

    // ── Workspace filesystem ─────────────────────────────────
    let workspace = tools::WorkspaceFs::new(workspace_root, sandbox_config).unwrap_or_else(|e| {
        eprintln!(
            "ERROR: Cannot create workspace at {}: {e}",
            workspace_root.display()
        );
        std::process::exit(1);
    });
    let workspace = Arc::new(workspace);

    // ── Tool registry ────────────────────────────────────────
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CalculatorTool));
    registry.register(Arc::new(ReadTool::new(workspace.clone())));
    registry.register(Arc::new(EditTool::new(workspace.clone())));
    registry.register(Arc::new(WriteTool::new(workspace.clone())));
    registry.register(Arc::new(GlobTool::new(workspace.clone())));
    registry.register(Arc::new(GrepTool::new(workspace.clone())));
    registry.register(Arc::new(LsTool::new(workspace.clone())));
    registry.register(Arc::new(ShellTool::new(
        workspace_root.to_path_buf(),
        sandbox_config,
    )));

    let tool_names: Vec<String> = registry.iter().map(|(n, _)| n.to_string()).collect();
    let registry = Arc::new(registry);

    // ── Memory ───────────────────────────────────────────────
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

    // ── LLM Client ────────────────────────────────────────────
    let client = DeepSeekClient::new(api_key);

    // ── Sandbox components ────────────────────────────────────
    let shell_filter = ShellFilter::from_config(sandbox_config);
    let resource_tracker = Arc::new(ResourceTracker::new(sandbox_config));
    let audit_logger = Arc::new(AuditLogger::new(sandbox_config, workspace_root));

    // ── Hooks ─────────────────────────────────────────────────
    // SandboxHook — shell approval, resource tracking, audit logging
    let (approval_hook, approval_tx) =
        SandboxHook::new(shell_filter, resource_tracker, audit_logger);
    approval_hook.set_hook_tx(hook_tx.clone());

    // MicroCompactHook — clears old tool output content (sync AgentHook)
    let micro_compact = hooks::MicroCompactHook::new(
        hooks::DEFAULT_KEEP_RECENT_TOOL_OUTPUTS,
        hooks::DEFAULT_COMPACTABLE_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );

    let hooks: Vec<Box<dyn engine::AgentHook>> =
        vec![Box::new(micro_compact), Box::new(approval_hook)];

    // Macro-compaction config — LLM summarisation when over budget.
    // The agent loop handles the async LLM call directly.
    let macro_compact = engine::MacroCompactConfig {
        model: flash_model.to_string(),
        threshold: hooks::DEFAULT_COMPACT_CHARS,
        keep_last_n: hooks::DEFAULT_KEEP_LAST_N,
    };

    // ── Engine context (via builder) ─────────────────────────
    let ctx = EngineContext::builder(client, memory.clone(), registry, model.to_string())
        .hooks(hooks)
        .max_steps(50)
        .max_retries(3)
        .streaming(true)
        .macro_compact(macro_compact)
        .build();

    let agent = Agent::new(ctx);

    // ── Seed system prompt ────────────────────────────────────
    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::System, SYSTEM_PROMPT));
    }

    AgentKit {
        agent,
        memory,
        tool_names,
        model: model.to_string(),
        agent_rx,
        agent_tx,
        hook_rx,
        hook_tx,
        approval_tx,
    }
}
