# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build                        # debug build (all crates)
cargo build --release              # release build
cargo build -p loomis              # build just the binary
cargo test --all                   # run all tests
cargo test -p provider             # run provider crate tests
cargo test -p loomis               # run loomis (binary) tests
cargo clippy --all                 # lint all crates
cargo run -p loomis                # launch the TUI
```

Set `DEEPSEEK_API` in `.env` before running — `dotenvy` loads it at startup.

## Architecture

**Rust agent framework** (Rust 2024 edition, Tokio async). Cargo workspace named `agent_oxide`.

**Rust edition**: Uses Rust 2024 with native async fn in traits (RPITIT). Do NOT use `async-trait` crate. Prefer sync traits for dyn-dispatch; keep async work in dedicated components.

### Workspace structure

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["libs/*", "bins/*"]
├── libs/
│   ├── provider/           # LLMClient trait + shared types (Message, ToolCall, ToolDef, etc.)
│   ├── deepseek/           # DeepSeekClient — implements LLMClient with SSE streaming
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs sandbox, ProgressStream
│   ├── tools-macros/       # #[tool] proc macro — generates Tool trait impls
│   ├── memory/             # Memory buffer, PendingHints, conversation persistence
│   ├── hooks/              # MicroCompactHook + MacroCompactHook (AgentHook impls)
│   ├── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent stream, ResponseRouter
│   ├── subagent/           # SubagentTool — spawn child agents as tools
│   ├── observability/      # TraceEvent, TraceStore, RunMetrics — full-chain agent tracing
│   └── skills/            # SkillDef, SkillRegistry, YAML frontmatter parser — skill discovery
├── bins/
│   └── loomis/             # Binary — concrete tools, hooks, sandbox, TUI, assembly
└── docs/
    ├── beginner-developer-guide.md
    ├── senior-developer-guide.md
    └── sandbox-architecture.md
```

### Crate map

| Crate | Role | Key types |
| --- | --- | --- |
| `provider` | Abstraction | `LLMClient` trait, `Message`, `Role`, `ToolCall`, `ToolCallKind`, `ToolDef`, `CompletionRequest`/`Response`, `ProviderError`, `StreamChunk`, `Delta`, `FinishReason`, `Usage` |
| `deepseek` | Provider impl | `DeepSeekClient` (impl `LLMClient`), SSE streaming parser |
| `tools` | Abstraction | `Tool` trait (sync, `Send+Sync`), `ToolRegistry`, `WorkspaceFs`, `SandboxConfig`, `Progress`, `ProgressStream`, `ToolError` |
| `tools-macros` | Proc macro | `#[tool]` attribute — generates `Tool` trait impl |
| `memory` | Abstraction | `Memory`, `SharedMemory`, `PendingHints`, `PersistenceConfig`, `ThreadInfo`, persistence fns |
| `hooks` | Concrete hooks | `MicroCompactHook`, `MacroCompactHook<C>`, compaction constants |
| `engine` | Core loop | `Agent`, `AgentBuilder`, `AgentHook` trait, `AgentEvent`, `AgentError`, `EngineContext`, `ResponseRouter`, `InterventionRequest`/`Response`, `RunOutcome`, `CallOrigin` |
| `subagent` | Concrete | `SubagentTool<C>`, `SubagentConfig`, `filter_tools()` |
| `observability` | Abstraction | `TraceEvent`, `TraceStore`, `RunMetrics`, `SubagentTrace`, `Timestamped<T>` |
| `skills` | Abstraction | `SkillDef`, `SkillRegistry`, `ActiveSkills`, YAML frontmatter parser, `SkillError` |
| `loomis` | Binary | 12 concrete tools, 7 hooks (Sandbox, Persistence, SystemPrompt, TodoList, Observability, PlanMode, Skill), sandbox system, TUI, `AgentKit`, `build_coding_agent()` |

### Dependency graph

```text
provider (no internal deps)
    ↑
    ├── deepseek ──────── (impl LLMClient)
    ├── tools ─────────── (uses ToolDef)
    ├── memory ────────── (uses Message)
    ↑
    ├── engine ────────── (uses provider + tools + memory)
    │       ↑
    ├── hooks ─────────── (uses provider + memory + engine)
    │       ↑
    ├── subagent ──────── (uses provider + tools + engine + memory + observability)
    │       ↑
    ├── observability ─── (uses provider)
    │       ↑
    ├── skills ─────────── (no internal deps — pure data types + filesystem IO)
    │       ↑
    └── loomis (bin) ──── (uses all libs)
```

## Key patterns

### `LLMClient` trait
Abstraction over LLM providers. Uses Rust 2024 native async fn, NOT `#[async_trait]`. `DeepSeekClient` is the reference implementation.

### `Tool` trait
Sync and object-safe. `execute_stream()` returns `ProgressStream` — short tools emit a single `Progress::Done`, long-running tools (shell) emit `Progress::InProgress` updates then `Progress::Done`. Use `tokio::sync::mpsc` from a spawned thread for async I/O.

### `AgentHook` trait
10 lifecycle callbacks, all with default no-ops. Naming convention:

| Prefix | Meaning |
| --- | --- |
| `on_<event>` | Pure notification — cannot intervene |
| `before_<action>` | Can intervene — return `Err` to block |
| `after_<action>` | Observe result — cannot intervene |

Callbacks: `on_run_start`, `on_run_finish(&RunOutcome)`, `on_step_start`, `on_llm_start(&SharedMemory)`, `on_llm_end(&Message)`, `on_llm_error(&ProviderError, attempt, will_retry)`, `before_tool_call`, `after_tool_call`, `on_tool_failed`.

Hooks run in registration order. For async work inside sync hooks (e.g. LLM summarisation), use `tokio::runtime::Handle::block_on` — the agent loop runs in a dedicated tokio task separate from the TUI thread.

Concrete hooks:
- **`hooks` crate**: `MicroCompactHook` (tool-output clearing), `MacroCompactHook<C>` (LLM summarisation)
- **`loomis` crate**: `SandboxHook` (security), `PersistenceHook` (auto-save), `SystemPromptHook` (seed prompts), `TodoListHook` (sync todo state), `ObservabilityHook` (full-chain trace collection), `SkillHook` (maintain `[SKILL: ...]` System messages), `PlanModeHook` (tool restriction + prompt injection)

### `AgentEvent` stream
Single `mpsc::unbounded_channel`. Variants:

| Event | When |
| --- | --- |
| `RunStarted { session_id, user_input }` | New task begins |
| `Token(String)` / `ReasoningToken(String)` | LLM output streaming |
| `ToolCallStart { id, name }` | Tool name known before args — immediate UI feedback |
| `ToolCall { id, name, arguments, origin }` | Before tool execution |
| `ToolSuccessful { id, name, output }` | Tool completed |
| `ToolRejected { id, name, reason }` | Hook blocked tool |
| `ToolFailure { id, name, error }` | Tool execution failed |
| `ToolProgress { id, name, message }` | Real-time progress from tool |
| `InterventionRequired(InterventionRequest)` | Hook needs user decision |
| `RunCompleted { answer }` | Success |
| `RunFailed { error }` | Error |
| `Cancelled` | User cancelled |
| `Done` | Sentinel — always last |

`CallOrigin::Llm` vs `CallOrigin::User` distinguishes LLM tool calls from user `!command` invocations.

### `AgentBuilder` vs `EngineContextBuilder`
- `Agent::builder(client, model)` — simple API: auto-creates Memory, seeds system prompt, collects tools into ToolRegistry.
- `EngineContext::builder(client, memory, tools, model)` — advanced API: supply Memory and ToolRegistry explicitly, configure hooks, max_steps, max_retries, streaming, pending_hints.

### Two-tier compaction
Both in the `hooks` crate:
1. **MicroCompact** — `on_llm_start()` clears old tool outputs from high-volume tools (read, shell, grep, glob, edit, write, ls) in-place.
2. **MacroCompact** — `on_llm_start()` checks `total_chars()`; when over threshold, drains old non-System messages (keeping last N), calls a compact model for summarisation via `block_on`, inserts summary as System message.

### Sandbox (defense in depth)

| Layer | Component | Role |
| --- | --- | --- |
| 1 | `WorkspaceFs` | Path sandbox — canonicalization, file-size caps, extension blocklist, hidden-file protection, binary detection, TOCTOU re-check |
| 2 | `ShellFilter` | Command classification — auto-approve (prefixes like `git`, `cargo`), deny (patterns like `rm -rf /`, `sudo`), prompt user for rest. Strict allowlist mode via `allowed_commands.binaries`. Runs in both `before_tool_call` and `execute_stream` |
| 3 | `SandboxHook` | Orchestrator — checks quotas, classifies commands, prompts user via `InterventionRequired`, updates resource counters, logs to `AuditLogger`. Uses `ResponseRouter` + rendezvous channel for blocking approval |
| 4 | `EnvSanitizer` | Clears dangerous env vars before spawning child processes |
| 5 | Watchdog | Kills process tree on timeout (`taskkill /F /T` on Windows) |

Config: `.loomis/config.toml` → `SandboxConfig` (safe defaults if missing).

### Observability (full-chain tracing)

`ObservabilityHook` implements all 9 `AgentHook` callbacks to capture lifecycle events with timing data and token counts. Events flow through a side channel (`Arc<TraceStore>`) shared between agent task and TUI — no changes to `AgentEvent` enum needed.

| Component | Crate | Role |
| --- | --- | --- |
| `TraceEvent` | `observability` | 12 variants: `RunStarted`, `RunFinished`, `StepStarted`, `LlmCallStarted`, `LlmCallFinished`, `LlmCallFailed`, `ToolCallStarted`, `ToolCallFinished`, `ToolCallRejected`, `StreamingSummary`, `SubagentFinished` |
| `TraceStore` | `observability` | Thread-safe ring buffer (4096 entries), lock-free `RunMetrics` atomics, `export_jsonl()` |
| `RunMetrics` | `observability` | Aggregated counters (steps, LLM calls, tool calls, tokens, errors) — read by TUI status bar |
| `ObservabilityHook` | `loomis` | Implements `AgentHook`, captures `Instant::now()` at each phase boundary, holds `SharedMemory` to read `last_usage` in `on_llm_end` |
| `DebugOverlay` | `loomis` (TUI) | Toggleable via `Ctrl+O` or `/debug`, scrollable table of recent trace events, synced each frame |

**Data flow**: Agent loop → `AgentHook` callbacks → `ObservabilityHook::emit(TraceEvent)` → `TraceStore` ring buffer (hot path, O(1), no alloc) → TUI drains at 20fps via `App::sync_trace()`.

**Subagent tracing**: `SubagentTool` holds `Option<Arc<TraceStore>>`. On child completion, `emit_subagent_trace()` reads child memory's `usage_history` and emits a single `SubagentFinished` event to the parent's store.

**Persistence**: `/trace-save` slash command exports buffered events as JSONL to `.loomis/traces/trace_{timestamp}.jsonl`.

### Plan Mode (read-only research & planning)

Toggled via `/plan` slash command. When active, the agent switches to a read-only mode — it can explore the codebase but cannot make changes.

| Component | Crate | Role |
| --- | --- | --- |
| `PlanModeState` | `loomis` (hooks) | `Arc<AtomicBool>` shared between TUI and `PlanModeHook` |
| `PlanModeHook` | `loomis` (hooks) | Implements `AgentHook` — `before_tool_call` blocks write/edit/shell (except plan file), `on_llm_start` injects `[PLAN_MODE]` System message |
| `/plan` | `loomis` (TUI) | Toggle slash command — flips `PlanModeState::active`, creates `.loomis/plan.md` |
| `/approve` | `loomis` (TUI) | Exits plan mode with an "approved" message |

**Hook order**: PlanModeHook runs at position 1 (after SystemPromptHook, before ObservabilityHook) so plan-mode rejections happen before compaction or sandbox evaluation.

**Allowed tools in plan mode**: `read`, `ls`, `glob`, `grep`, `calculator`, `ask_user_question`, `todo`, `task`/`subagent`, `write` (only to `.loomis/plan.md`).

**Blocked tools in plan mode**: `edit`, `shell`, `write` (any file other than the plan file).

**Plan file**: `.loomis/plan.md` — the only writable file in plan mode. The LLM is instructed to write its plan here, then ask the user to review and toggle plan mode off.

### Hook registration order

Hooks run in this order (set in `build_coding_agent()`):

| Pos | Hook | Role |
| --- | --- | --- |
| 0 | `SystemPromptHook` | Seeds initial system prompts (now includes `{skill_list}`) |
| 1 | `PlanModeHook` | Plan mode tool restrictions + `[PLAN_MODE]` prompt injection |
| 2 | `ObservabilityHook` | Full-chain trace event collection |
| 3 | `PersistenceHook` | Auto-save conversation after each run |
| 4 | `TodoListHook` | Maintain `[TODO]` System message |
| 5 | `SkillHook` | Maintain `[SKILL: ...]` System messages |
| 6 | `MacroCompactHook` | LLM summarisation when over token budget |
| 7 | `MicroCompactHook` | Clear old tool outputs in-place |
| 8 | `SandboxHook` | Security sandbox (shell approval, quotas, audit) |

### Skills (LLM self-selecting instructions)

Skill `.md` files (YAML frontmatter + Markdown body) are discovered at startup, listed in the system prompt, and loaded on demand by the LLM or user into the conversation as `[SKILL: name]` System messages.

| Component | Crate | Role |
| --- | --- | --- |
| `SkillDef` | `skills` | Parsed skill: `name`, `description`, `content` |
| `SkillRegistry` | `skills` | Load-once registry — `discover(&[PathBuf])` scans `*.md` files, deduplicates (later paths win) |
| `SkillFrontmatter` | `skills` | `serde_yaml` deserialization target for `---\nname: ...\ndescription: ...\n---` |
| `ActiveSkills` | `skills` | `Arc<RwLock<HashMap<String, String>>>` — shared between `SkillTool` (writer) and `SkillHook` (reader) |
| `SkillTool` | `loomis` | LLM-callable tool — looks up skill by name in registry, writes to `ActiveSkills`, returns content |
| `SkillHook` | `loomis` | `on_llm_start`: clones `ActiveSkills`, `retain()` to remove old `[SKILL:` messages, inserts one per active skill at index 0 |
| `/skill <name>` | `loomis` (TUI) | Slash command — loads a skill directly, writes to `ActiveSkills` + injects into memory |

**Data flow**: `.md` files → `SkillRegistry::discover()` at startup → `{skill_list}` in system prompt → LLM calls `skill(name="...")` → `SkillTool` writes to `ActiveSkills` → next `on_llm_start`, `SkillHook` injects `[SKILL: name]\n\n{content}` System messages.

**Search paths** (configured in binary crate, passed to library):

1. `<workspace>/.loomis/skills/` (project — higher priority)
2. `~/.loomis/skills/` (user)

**Skill file format**:

```markdown
---
name: my-skill
description: What this skill does (one line)
---

# Skill instructions (Markdown body)

These instructions are injected as a System message when loaded.
```

**Marker convention**: `[SKILL:` prefix — follows the same `retain()`-based pattern as `[PLAN_MODE]` and `[TODO]`.

**Allowed in plan mode**: The `skill` tool runs in plan mode (it's read-only and only affects conversation instructions).

### `ResponseRouter`
Maps `request_id` → `SyncSender<InterventionResponse>`. Multiple components (SandboxHook, AskUserQuestionTool) can need user intervention simultaneously — each registers its own channel, sends an `InterventionRequired` event with its `request_id`, and blocks on its receiver. The TUI routes responses through the router.

### `!command` shell execution
User-typed `!` prefix in TUI runs commands, pushes output to `SharedMemory`, emits unified `ToolCall { origin: User }` / `ToolSuccessful` events.

### Conversation persistence
Auto-saves to `.loomis/threads/{name}.json` + `.md` after each agent turn via `PersistenceHook`. Thread picker via `/resume`. Date suffixes on thread names for uniqueness.

### Subagent
`SubagentTool<C>` wraps a child `Agent` as a `Tool`. Spawned with filtered (typically read-only) tool set and its own Memory. Results streamed as `Progress::InProgress`/`Progress::Done`. Config: `SubagentConfig` (model, max_steps, timeout_secs, inherit_context_messages).

### Loomis concrete tools (12)
`Calculator`, `Read`, `Edit`, `Write`, `Glob`, `Grep`, `Ls`, `Shell`, `Subagent`, `AskUserQuestion`, `Todo`, `Skill`

### Loomis concrete hooks (7)
`SystemPromptHook` (seeds initial system messages, now includes `{skill_list}`), `PlanModeHook` (tool restriction + plan-mode prompt injection), `ObservabilityHook` (full-chain trace collection), `PersistenceHook` (auto-save), `TodoListHook` (syncs [TODO] System message), `SkillHook` (maintains `[SKILL: ...]` System messages from shared `ActiveSkills` state), `SandboxHook` (security)

### TUI module (`bins/loomis/src/tui/`)

ratatui + crossterm. Channel topology:

```text
TUI thread                          Agent task (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

- **Keybindings**: Enter (submit), Ctrl+C (cancel), Esc (cancel), Ctrl+D (exit), Ctrl+O (toggle trace debug overlay), PgUp/PgDown (scroll), Up/Down (history), Left/Right/Home/End (cursor). Intervention prompts: ↑/↓ (navigate), Enter (select), Esc (cancel)
- **Slash commands**: `/exit`, `/new`, `/plan`, `/approve`, `/save <name>`, `/resume [name]`, `/threads`, `/stats`, `/tools`, `/debug`, `/trace-save`, `/skill <name>`, `/help`
- **Bang prefix**: `!command` — runs shell, output shared with agent

## Future work

- [ ] Publish lib crates to crates.io
- [ ] Add `libs/openai/`, `libs/anthropic/` provider implementations
- [ ] RAG/vector DB support
