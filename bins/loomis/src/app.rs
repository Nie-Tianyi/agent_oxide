//! Agent assembly — wires all components together.

use std::path::Path;
use std::sync::Arc;

use deepseek::DeepSeekClient;
use engine::{Agent, EngineContext};
use hooks;
use memory::{Memory, SharedMemory};
use provider::{Message, Role};
use subagent::{self, SubagentConfig};
use tokio::sync::mpsc;
use tools::ToolRegistry;

use tools::SandboxConfig;

use crate::hooks::ResponseRouter;
use crate::hooks::SandboxHook;
use crate::sandbox::audit_logger::AuditLogger;
use crate::sandbox::resource_tracker::ResourceTracker;
use crate::sandbox::shell_filter::ShellFilter;
use crate::tools::{
    AskUserQuestionTool, CalculatorTool, EditTool, GlobTool, GrepTool, LsTool, ReadTool, ShellTool,
    WriteTool,
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

9. **Delegate complex work.** Use the `task` tool to spawn multiple sub-agents for \
multi-step investigation, code analysis, or refactoring that can run in parallel. The sub-agent has \
read-only tools (read, ls, glob, grep, calculator) and works independently. \
Be specific in your description and prompt — the sub-agent works independently \
and reports back.
";

// ── AgentEvent & InterventionResponse (re-exported from engine) ─────────────────

/// Re-export the engine's event type for channel construction.
pub use engine::AgentEvent;
pub use engine::InterventionResponse;

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
    /// Routes intervention responses to the correct requester
    /// (SandboxHook, AskUserQuestionTool, …).
    pub response_router: Arc<ResponseRouter>,
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

    // ── Workspace filesystem ─────────────────────────────────
    let workspace = tools::WorkspaceFs::new(workspace_root, sandbox_config).unwrap_or_else(|e| {
        eprintln!(
            "ERROR: Cannot create workspace at {}: {e}",
            workspace_root.display()
        );
        std::process::exit(1);
    });
    let workspace = Arc::new(workspace);

    // ── Shared intervention response router ───────────────────
    // Must be created before tools — AskUserQuestionTool needs it.
    let response_router = Arc::new(ResponseRouter::new());

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

    // ── Memory ───────────────────────────────────────────────
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

    // ── LLM Clients ─────────────────────────────────────────────
    let client = DeepSeekClient::new(api_key);
    let subagent_client = client.clone(); // clone before client is moved into EngineContext
    let compact_client = DeepSeekClient::new(api_key);

    // ── Subagent tool (read-only subset, no shell, no write, no task) ──
    let subagent_registry =
        subagent::filter_tools(&registry, &["read", "ls", "glob", "grep", "calculator"]);
    let subagent_registry = Arc::new(subagent_registry);

    let subagent_config = SubagentConfig {
        model: flash_model.to_string(),
        ..Default::default()
    };
    let subagent_tool = subagent::SubagentTool::new(
        subagent_client,
        subagent_config,
        subagent_registry,
        memory.clone(),
    );
    registry.register(Arc::new(subagent_tool));

    // AskUserQuestionTool — lets the LLM ask the user questions.
    let ask_tool = AskUserQuestionTool::new(response_router.clone());
    ask_tool.set_agent_tx(agent_tx.clone());
    registry.register(Arc::new(ask_tool));

    let tool_names: Vec<String> = registry.iter().map(|(n, _)| n.to_string()).collect();
    let registry = Arc::new(registry);

    // ── Sandbox components ────────────────────────────────────
    let shell_filter = ShellFilter::from_config(sandbox_config);
    let resource_tracker = Arc::new(ResourceTracker::new(sandbox_config));
    let audit_logger = Arc::new(AuditLogger::new(sandbox_config, workspace_root));

    // ── Hooks ─────────────────────────────────────────────────
    // SandboxHook — shell approval, resource tracking, audit logging
    let approval_hook = SandboxHook::new(
        shell_filter,
        resource_tracker,
        audit_logger,
        response_router.clone(),
    );
    approval_hook.set_agent_tx(agent_tx.clone());

    // MicroCompactHook — clears old tool output content
    let micro_compact = hooks::MicroCompactHook::new(
        hooks::DEFAULT_KEEP_RECENT_TOOL_OUTPUTS,
        hooks::DEFAULT_COMPACT_ELIGIBLE_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );

    // MacroCompactHook — LLM summarisation when over budget.
    // Blocks the agent task via Handle::block_on (separate thread from TUI).
    let macro_compact = hooks::MacroCompactHook::new(
        flash_model.to_string(),
        hooks::DEFAULT_COMPACT_CHAR_LIMIT,
        hooks::DEFAULT_KEEP_LAST_N,
        compact_client,
    );

    let hooks: Vec<Box<dyn engine::AgentHook>> = vec![
        Box::new(macro_compact),
        Box::new(micro_compact),
        Box::new(approval_hook),
    ];

    // ── Engine context (via builder) ─────────────────────────
    let ctx = EngineContext::builder(client, memory.clone(), registry, model.to_string())
        .hooks(hooks)
        .max_steps(50)
        .max_retries(3)
        .streaming(true)
        .build();

    let agent = Agent::new(ctx);

    // ── Seed system prompt ────────────────────────────────────
    {
        let mut mem = memory.write().expect("memory lock poisoned");
        mem.push(Message::new(Role::System, SYSTEM_PROMPT));
    }

    AgentKit {
        agent,
        memory,
        tool_names,
        model: model.to_string(),
        agent_rx,
        agent_tx,
        response_router,
    }
}
