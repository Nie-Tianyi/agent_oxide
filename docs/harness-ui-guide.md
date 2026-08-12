# Building the Harness and UI Layers

> **Goal:** build the two user-owned layers of the four-layer architecture —
> the **Harness** (multi-agent orchestration, SKILLs, plugins) and the **UI**
> (TUI / GUI / CLI / WebUI) — with the ordinary core APIs: `Agent::builder`,
> hooks, the `AgentEvent` stream, and `ResponseRouter`. No proc macros
> involved.

| Layer | Contents | Built here |
| ----- | -------- | ---------- |
| 1 — UI | TUI / GUI / CLI / WebUI | this guide, Part II |
| 2 — Harness | multi-agent orchestration, SKILLs, plugins | this guide, Part I |
| 3 — Agent core | memory, tools, agent loop, hooks | provided by `agent_oxide` |
| 4 — LLM client | `LLMClient` trait, DeepSeek client | provided by `agent_oxide` |

The core APIs you will use:

- `Agent::builder(client, model)` / `EngineContext::builder(...)` — assemble
  an agent with tools, memory, hooks, and loop limits.
- `AgentHook` — the 9-lifecycle-callback plugin interface.
- `AgentEvent` — the streaming event channel that drives any UI.
- `ResponseRouter` — routes user decisions back to blocking hooks/tools.
- `SkillRegistry` / `SubagentTool` / `SharedMemory` — harness building blocks.

## Part I — The Harness layer

The harness is the policy layer between the UI and the agent core: which
tools an agent gets, which plugins run, how skills are loaded, and how
multiple agents cooperate. Everything below is assembled with
`Agent::builder`.

### 1. Plugins: hooks are your plugin point

```rust,ignore
use agent_oxide::prelude::*;

let agent = Agent::builder(client, "deepseek-chat")
    .system_prompt("You are a coding assistant.")
    .tool(ReadTool::new(workspace))
    .tool(ShellTool::new())
    .hook(SandboxHook::new(sandbox_config))
    .hook(PersistenceHook::new(persistence_config))
    .max_steps(50)
    .build();
```

`AgentHook` has 9 callbacks, all default no-ops — sandbox, persistence,
observability, compaction, and your own extensions are all just hooks:

| Callback | Can intervene? |
| -------- | -------------- |
| `on_run_start` / `on_run_finish` | observe |
| `on_step_start` | observe |
| `on_llm_start` / `on_llm_end` / `on_llm_error` | observe |
| `before_tool_call` | **block** — return `Err` to reject the call |
| `after_tool_call` / `on_tool_failed` | observe |

Two rules to remember:

- Hooks run in **registration order** — order matters when hooks depend on
  each other (e.g. register sandbox after observability).
- For async work inside a sync hook (e.g. LLM summarisation), use
  `agent_oxide::engine::block_on` — a bare `Handle::block_on` panics on tokio worker
  threads.

A trivial plugin — a logging hook:

```rust,ignore
struct LoggingHook;

impl AgentHook for LoggingHook {
    fn on_run_start(&self, session_id: &str, user_input: &str, _memory: &SharedMemory) {
        println!("[{session_id}] run: {user_input}");
    }
    fn before_tool_call(&self, _session_id: &str, tool_call: &ToolCall) -> Result<(), AgentError> {
        println!("[tool] {}", tool_call.name);
        Ok(())
    }
}
```

### 2. SKILLs: SkillRegistry

Skills are `.md` files (YAML frontmatter + body) discovered at startup:

```rust,ignore
use agent_oxide::skills::SkillRegistry;

let registry = SkillRegistry::discover(&[project_skills_dir, user_skills_dir]);
for skill in registry.list() {
    println!("{} — {}", skill.name, skill.description);
}
```

Each `SkillDef` carries `name`, `description`, and `content` (the markdown
body). The registry is provider-agnostic — wiring it into the agent is your
harness's job, typically two pieces:

1. a **`SkillTool`** the LLM can call to activate a skill
   (`registry.by_name(name)` → insert into `ActiveSkills`), and
2. a **`SkillHook`** that injects the activated skill's content as a System
   message before the next LLM call.

```rust,ignore
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use agent_oxide::skills::{ActiveSkills, SkillRegistry};

let active: ActiveSkills = Arc::new(RwLock::new(HashMap::new()));

// Tool side: "load_skill(name)" → active.insert(name, content)
let tool = LoadSkillTool { registry, active: active.clone() };

// Hook side: on_llm_start() → push each active skill's content as a System
// message (read the map under the lock; avoid holding it across the call).
let hook = SkillInjectHook { active };

let agent = Agent::builder(client, model)
    .tool(tool)
    .hook(hook)
    .build();
```

### 3. Multi-agent orchestration: subagents and shared memory

**Delegation** — `SubagentTool` spawns an isolated child agent for complex
sub-tasks. Give the child a **restricted tool set** (never the `task` tool
itself, to prevent recursion):

```rust,ignore
use agent_oxide::subagent::{SubagentConfig, SubagentTool};
use agent_oxide::subagent::filter_tools;

let child_tools = Arc::new(filter_tools(
    &parent_registry,
    &["read", "ls", "glob", "grep", "calculator"],
));

let task_tool = SubagentTool::new(
    client.clone(),          // cloned per invocation — children are independent
    SubagentConfig::default(),
    child_tools,
    parent_memory.clone(),   // child memory stays isolated
);

let agent = Agent::builder(client, model)
    .tool(task_tool)
    .memory(parent_memory)
    .build();
```

**Collaboration** — two agents sharing one `SharedMemory` take turns on the
same conversation; each run appends to the shared history:

```rust,ignore
use agent_oxide::memory::{Memory, SharedMemory};
use std::sync::{Arc, RwLock};

let shared: SharedMemory = Arc::new(RwLock::new(Memory::new()));

let researcher = Agent::builder(client.clone(), model)
    .memory(shared.clone())
    .system_prompt("You are a researcher. Summarize findings briefly.")
    .build();

let writer = Agent::builder(client, model)
    .memory(shared.clone())
    .system_prompt("You are a writer. Turn the research into prose.")
    .build();
```

### 4. A recommended full harness

Register hooks in policy order — the reference app uses: SystemPrompt →
Observability → Persistence → Skills → PlanMode → Profile → Todo → Compact
→ Sandbox. A production harness from the bundled extensions:

```rust,ignore
use agent_oxide::prelude::*;

let agent = Agent::builder(client, "deepseek-v4-pro")
    .system_prompt(harness_system_prompt)
    .tool(SubagentTool::new(...))
    .tool(LoadSkillTool::new(...))
    .hook(ObservabilityHook::new(trace_store))
    .hook(PersistenceHook::new(PersistenceConfig::default()))
    .hook(SkillInjectHook::new(active_skills))
    .hook(MicroCompactHook::default())
    .hook(MacroCompactHook::default())
    .hook(SandboxHook::new(SandboxConfig::load(&sandbox_path)))
    .max_steps(50)
    .build();
```

The same agent can then be exposed through any UI — which is Part II.

## Part II — The UI layer

The UI is a **pure consumer of `AgentEvent`**. It never touches memory,
tools, or hooks directly — that keeps TUI / GUI / CLI / WebUI
interchangeable against one harness.

### 5. The event stream

`Agent::run_with_events` emits every lifecycle event through a
`tokio::sync::mpsc::UnboundedSender<AgentEvent>`:

```rust,ignore
use tokio::sync::mpsc;

let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
let handle = tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
        // render the event…
    }
});

let answer = agent.run_with_events(user_input, tx).await?;
handle.await?;
```

The events you will render:

| Event | UI meaning |
| ----- | ---------- |
| `RunStarted { session_id, user_input }` | new turn begins |
| `Token(String)` / `ReasoningToken(String)` | streaming text |
| `ToolCallStart { id, name }` | tool name known |
| `ToolCall` / `ToolSuccessful` / `ToolFailure` | tool lifecycle |
| `ToolProgress { id, name, message }` | real-time progress lines |
| `InterventionRequired(InterventionRequest)` | **user decision needed** |
| `RunCompleted { answer }` / `RunFailed { error }` | turn outcome |
| `Done` | sentinel — always last |

### 6. A minimal CLI

```rust,ignore
use agent_oxide::prelude::*;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = DeepSeekClient::new(std::env::var("DEEPSEEK_API")?);

    let agent = Agent::builder(client, "deepseek-chat")
        .system_prompt("You are a helpful assistant.")
        .build();

    let router = Arc::new(ResponseRouter::new());
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    // UI task — consumes events, never touches the agent.
    let ui_router = Arc::clone(&router);
    let ui = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::Token(text) => print!("{text}"),
                AgentEvent::ToolCallStart { name, .. } => println!("\n[tool] {name} …"),
                AgentEvent::ToolProgress { message, .. } => println!("        {message}"),
                AgentEvent::InterventionRequired(req) => {
                    println!("\n{0}: {1}", req.title, req.description);
                    for (i, option) in req.options.iter().enumerate() {
                        println!("  {i}. {option}");
                    }
                    // Read the user's choice on stdin, then route it back
                    // (shown simplified; a real CLI validates the input).
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).ok();
                    let chosen = line.trim().parse::<usize>().ok();
                    ui_router.route(
                        &req.request_id,
                        InterventionResponse { chosen, custom_text: None },
                    );
                }
                AgentEvent::RunCompleted { .. } => println!("\n[completed]"),
                AgentEvent::Done => break,
                _ => {}
            }
        }
    });

    // Main task — runs the agent.
    let answer = agent
        .run_with_events("Draft a release checklist for v0.6.", tx)
        .await?;
    println!("\n{answer}");
    ui.await?;
    Ok(())
}
```

### 7. How interventions work end to end

When a hook or tool needs a decision (sandbox approval, an LLM question), it:

1. generates a `request_id` (`agent_oxide::engine::next_request_id()`),
2. registers a `std::sync::mpsc::SyncSender` with the `ResponseRouter`,
3. emits `InterventionRequired(request)` and blocks on its own receiver.

The UI receives the event, renders the request, and unblocks the requester:

```rust,ignore
// Requester side (sandbox hook, AskUserQuestion tool):
let (tx_sync, rx_sync) = std::sync::mpsc::sync_channel(0);
router.register(request_id.clone(), tx_sync);
tx_events.send(AgentEvent::InterventionRequired(request)).ok();
let response = rx_sync.recv(); // blocks until the UI routes a response

// UI side:
router.route(&request_id, InterventionResponse {
    chosen: Some(2),           // index into request.options
    custom_text: None,         // or Some(text) for a "…" option
});
```

### 8. Rendering directions

- **CLI** — `println!` over the stream, stdin for interventions (this guide).
- **TUI** — render tokens into a text pane and tool progress into a status
  bar (ratatui-style); route choices through the `ResponseRouter`.
- **WebUI** — forward events to the browser over WebSocket / SSE; the run
  task stays server-side, one event loop per session.
- **GUI** — same stream, any widget framework; keep rendering in the UI
  thread and route decisions back on it.

Whatever the frontend, the harness and the agent stay untouched — swap the
UI by replacing only the event consumer.

## Recap

- **Harness** = `Agent::builder` + hooks (plugins) + `SkillRegistry`
  (SKILLs) + `SubagentTool` / shared memory (multi-agent orchestration).
- **UI** = `run_with_events` + `ResponseRouter` (interventions), a pure
  consumer of `AgentEvent`.

Both layers are consumer code on top of the core APIs — see
[`agent-kit-guide.md`](agent-kit-guide.md) for the macro-based alternative,
and the `README.md` four-layer table for where everything fits.
