//! Agent assembly — wires all components together.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use deepseek::DeepSeekClient;
use engine::{Agent, EngineContext};
use hooks;
use memory::{Memory, PendingHints, PersistenceConfig, SharedMemory};
use observability::TraceStore;
use skills::{self, SkillRegistry};
use subagent::{self, SubagentConfig};
use tokio::sync::mpsc;
use tools::ToolRegistry;

use tools::SandboxConfig;

use crate::hooks::{
    ObservabilityHook, PersistenceHook, PlanModeHook, PlanModeState, SandboxHook, SkillHook,
    SystemPromptHook, TodoListHook,
};
use crate::sandbox::audit_logger::AuditLogger;
use crate::sandbox::resource_tracker::ResourceTracker;
use crate::sandbox::shell_filter::ShellFilter;
use crate::tools::{
    AskUserQuestionTool, CalculatorTool, EditTool, EnterPlanModeTool, ExitPlanModeTool, GlobTool,
    GrepTool, LsTool, ReadTool, ShellTool, SkillTool, TodoItem, TodoTool, WriteTool,
};
use engine::ResponseRouter;

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
    /// Queue for user hints injected during active agent runs.
    /// Drained by the agent loop before each LLM call.
    pub pending_hints: PendingHints,
    /// Persistence config — directory layout and naming for thread storage.
    pub persistence_config: PersistenceConfig,
    /// Shared todo list state — written by [`TodoTool`], read by the TUI status bar.
    pub todos: Arc<RwLock<Vec<TodoItem>>>,
    /// Shared trace store — written by [`ObservabilityHook`], read by the TUI.
    pub trace_store: Arc<TraceStore>,
    /// Shared plan-mode toggle between TUI and [`PlanModeHook`].
    pub plan_mode: Arc<PlanModeState>,
    /// Discovered skills — read-only after startup.
    pub skill_registry: Arc<SkillRegistry>,
    /// Currently active skills — written by [`SkillTool`], read by [`SkillHook`].
    pub active_skills: skills::ActiveSkills,
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
        tracing::error!(
            path = %workspace_root.display(),
            error = %e,
            "Cannot create workspace",
        );
        std::process::exit(1);
    });
    let workspace = Arc::new(workspace);

    // ── Shared intervention response router ───────────────────
    // Must be created before tools — AskUserQuestionTool needs it.
    let response_router = Arc::new(ResponseRouter::new());

    // ── Pending hints queue ────────────────────────────────────
    // Decouples user hint injection from memory mutation so hints
    // never land between an assistant tool_calls message and its
    // tool results (which violates the provider API contract).
    let pending_hints = PendingHints::default();

    // ── Plan mode state ───────────────────────────────────────
    // Created before tools so EnterPlanModeTool / ExitPlanModeTool
    // can be registered and included in tool_names.
    let plan_mode = Arc::new(PlanModeState::new());
    let plan_file_path = workspace_root.join(".loomis").join("plan.md");

    // ── Skills ────────────────────────────────────────────────
    // Discover skills from project and user directories.
    let skill_search_paths = vec![
        workspace_root.join(".loomis").join("skills"),
        dirs_fallback().join(".loomis").join("skills"),
    ];
    let skill_registry = Arc::new(SkillRegistry::discover(&skill_search_paths));
    let active_skills: skills::ActiveSkills = Arc::new(RwLock::new(HashMap::new()));

    // ── Tool registry ────────────────────────────────────────
    let mut registry = ToolRegistry::new();

    // Shared todo-list state — the TodoTool writes it, the TUI reads it.
    let todo_state = Arc::new(RwLock::new(Vec::<TodoItem>::new()));

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

    // ── Trace store (observability) ──────────────────────────
    let trace_store = Arc::new(TraceStore::new());

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
    )
    .with_trace_store(trace_store.clone());
    registry.register(Arc::new(subagent_tool));

    // AskUserQuestionTool — lets the LLM ask the user questions.
    let ask_tool = AskUserQuestionTool::new(response_router.clone());
    ask_tool.set_agent_tx(agent_tx.clone());
    registry.register(Arc::new(ask_tool));

    // TodoTool — lets the LLM manage a structured task list (plan).
    let todo_tool = TodoTool::new(todo_state.clone());
    registry.register(Arc::new(todo_tool));

    // EnterPlanModeTool — lets the LLM activate plan mode autonomously.
    let enter_plan_tool = EnterPlanModeTool::new(plan_mode.clone(), plan_file_path.clone());
    registry.register(Arc::new(enter_plan_tool));

    // ExitPlanModeTool — lets the LLM present the plan for user approval
    // and deactivate plan mode.
    let exit_plan_tool = ExitPlanModeTool::new(
        plan_mode.clone(),
        plan_file_path.clone(),
        response_router.clone(),
    );
    exit_plan_tool.set_agent_tx(agent_tx.clone());
    registry.register(Arc::new(exit_plan_tool));

    // SkillTool — lets the LLM load named skill instructions.
    registry.register(Arc::new(SkillTool::new(
        skill_registry.clone(),
        active_skills.clone(),
    )));

    let tool_names: Vec<String> = registry.iter().map(|(n, _)| n.to_string()).collect();
    let registry = Arc::new(registry);

    // ── Sandbox components ────────────────────────────────────
    let shell_filter = ShellFilter::from_config(sandbox_config);
    let resource_tracker = Arc::new(ResourceTracker::new(sandbox_config));
    let audit_logger = Arc::new(AuditLogger::new(sandbox_config, workspace_root));

    // ── Hooks ─────────────────────────────────────────────────

    // ObservabilityHook — full-chain trace event collection.
    let observability_hook = ObservabilityHook::new(trace_store.clone(), memory.clone());

    // PlanModeHook — restricts tools in plan mode, injects plan-mode prompt.
    let plan_mode_hook = PlanModeHook::new(
        plan_mode.clone(),
        plan_file_path.clone(),
        workspace_root.to_path_buf(),
    );

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
        hooks::DEFAULT_COMPACT_TOKEN_LIMIT,
        hooks::DEFAULT_KEEP_LAST_N,
        compact_client,
    );

    // SystemPromptHook — seeds the three initial system messages on first run
    // (now includes skill list in the main system prompt).
    let system_prompt_hook = SystemPromptHook::new(
        workspace_root.to_path_buf(),
        tool_names.clone(),
        skill_registry.clone(),
    );

    // TodoListHook — maintains the [TODO] System message from the shared
    // todo state.  Runs before compaction hooks so the message is present
    // in memory before any summarisation or clearing.
    let todo_list_hook = TodoListHook::new(todo_state.clone());

    // PersistenceHook — auto-saves conversation after each agent run.
    // Replaces the ad-hoc save in the TUI agent_handler's tokio::spawn block.
    let persistence_config = PersistenceConfig::default();
    let persistence_hook =
        PersistenceHook::new(workspace_root.to_path_buf(), persistence_config.clone());

    let hooks: Vec<Box<dyn engine::AgentHook>> = vec![
        Box::new(system_prompt_hook), // 0. Seed system prompts on run start
        Box::new(plan_mode_hook),     // 1. Plan mode filtering + prompt injection
        Box::new(observability_hook), // 2. Full-chain trace event collection
        Box::new(persistence_hook),   // 3. Save conversation after each run
        Box::new(todo_list_hook),     // 4. Maintain [TODO] System message
        Box::new(SkillHook::new(active_skills.clone())), // 5. Maintain [SKILL: ...] System messages
        Box::new(macro_compact),      // 6. LLM summarisation
        Box::new(micro_compact),      // 7. Tool output clearing
        Box::new(approval_hook),      // 8. Security sandbox
    ];

    // ── Engine context (via builder) ─────────────────────────
    let ctx = EngineContext::builder(client, memory.clone(), registry, model.to_string())
        .hooks(hooks)
        .max_steps(50)
        .max_retries(3)
        .streaming(true)
        .pending_hints(pending_hints.clone())
        .build();

    let agent = Agent::new(ctx);

    AgentKit {
        agent,
        memory,
        tool_names,
        model: model.to_string(),
        agent_rx,
        agent_tx,
        response_router,
        pending_hints,
        persistence_config,
        todos: todo_state,
        trace_store,
        plan_mode,
        skill_registry,
        active_skills,
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────────

/// Best-effort home directory — `HOME` on Unix, `USERPROFILE` on Windows.
fn dirs_fallback() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Ok(dir) = std::env::var("USERPROFILE") {
            return std::path::PathBuf::from(dir);
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(dir) = std::env::var("HOME") {
            return std::path::PathBuf::from(dir);
        }
    }
    std::path::PathBuf::from(".")
}
