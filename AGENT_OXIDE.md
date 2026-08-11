# AGENT_OXIDE.md

This file provides guidance to the Agent Oxide agent when working with code
in this repository.

## Build & Test

```bash
cargo build                        # debug build (umbrella crate)
cargo build --all                  # build all workspace crates
cargo build --release              # release build
cargo test --all                   # run all tests
cargo test -p engine               # one crate's tests
cargo test -p engine <name>        # single test by name substring
cargo clippy --all                 # lint all crates
```

Tests are **inline** (`#[cfg(test)] mod tests { ... }`) co-located with source
— no separate `tests/` directories.

## Architecture

**Rust agent framework** (Rust 2024 edition, Tokio async). The umbrella
`agent_oxide` crate re-exports all sub-crates; each sub-crate is also
published independently on crates.io.

**Rust edition**: Uses Rust 2024 with native async fn in traits (RPITIT).
Do NOT use `async-trait` crate. Prefer sync traits for dyn-dispatch; keep
async work in dedicated components.

### Workspace structure

```
agent_oxide/
├── Cargo.toml              # [package] agent_oxide umbrella + [workspace]
├── src/lib.rs              # umbrella re-exports + prelude
├── core/
│   ├── provider/           # LLMClient trait + shared types
│   ├── deepseek/           # DeepSeekClient — implements LLMClient
│   ├── tools/              # Tool trait, ToolRegistry, ProgressStream
│   ├── tools-macros/       # #[tool] proc macro
│   ├── memory/             # Memory buffer, PendingHints
│   ├── util/               # Shared workspace utilities (iso8601_now)
│   └── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent, ResponseRouter
├── extensions/
│   ├── skills/             # SkillDef, SkillRegistry — skill discovery & loading
│   ├── compact/            # hooks crate — MicroCompactHook + MacroCompactHook
│   ├── persistence/        # Conversation persistence — save/load threads, PersistenceHook
│   ├── subagent/           # SubagentTool — spawn child agents as tools
│   ├── observability/      # TraceEvent, TraceStore, RunMetrics — full-chain tracing
│   ├── sandbox/            # Sandbox runtime — WorkspaceFs, ShellFilter, SandboxHook, etc.
│   ├── agent-kit/          # NVIDIA OO Agents-style ergonomics
│   └── agent-macros/       # #[derive(Agent)] + #[agent_impl] proc macros
└── docs/
    ├── beginner-developer-guide.md
    ├── senior-developer-guide.md
    ├── sandbox-architecture.md
    └── agent-kit-guide.md
```

### Dependency graph

```text
core/
    provider (no internal deps)
        ↑
        ├── deepseek ──── (impl LLMClient)
        ├── tools ─────── (uses provider + tools-macros)
        ├── memory ────── (uses provider)
        ↑
        └── engine ────── (uses provider + tools + memory)
                ↑
extensions/
    skills ────────────── (no internal deps)
    hooks ─────────────── (uses provider + memory + engine)
    persistence ───────── (uses provider + engine + memory + deepseek + util)
    observability ─────── (uses provider + engine + memory)
    sandbox ───────────── (uses engine + memory + provider + util)
    subagent ──────────── (uses provider + tools + engine + memory + observability)
    agent-kit ─────────── (uses provider + tools + engine + memory)
    agent-macros ──────── (proc macro, no internal deps)
                ↑
umbrella
    agent_oxide ───────── (re-exports every crate)
```

## Key patterns

### `LLMClient` trait
Abstraction over LLM providers. Uses Rust 2024 native async fn (NOT
`#[async_trait]`). `DeepSeekClient` is the reference implementation.
Implement this trait to support a new provider.

### `Tool` trait
Sync and object-safe. `execute_stream()` returns `ProgressStream` — short
tools emit a single `Progress::Done`, long-running tools (shell) emit
`Progress::InProgress` updates then `Progress::Done`. Use
`tokio::sync::mpsc` from a spawned thread for async I/O.

### `#[tool]` proc macro
Annotate a struct with `#[tool(name = "...", description = "...", args = ArgsType)]`.
Generates `Tool` trait impl — the struct must define an inherent
`execute_stream(&self, args: ArgsType) -> Result<ProgressStream, ToolError>`.
JSON Schema is lazily generated from `ArgsType` via `schemars`.

> Note: the macro expands to `::tools::` / `::serde_json::` paths, so
> consuming crates need `tools` + `serde_json` as direct dependencies
> (standard proc-macro behaviour, like serde/tokio).

### `AgentHook` trait — 9 lifecycle callbacks
All have default no-ops. Naming convention:

| Prefix | Meaning |
| --- | --- |
| `on_<event>` | Pure notification — cannot intervene |
| `before_<action>` | Can intervene — return `Err` to block |
| `after_<action>` | Observe result — cannot intervene |

Callbacks (all receive `session_id: &str`):
- `on_run_start(&str, user_input: &str, memory: &SharedMemory)`
- `on_run_finish(&str, outcome: &RunOutcome, memory: &SharedMemory)`
- `on_step_start(&str, step: usize, max_steps: usize)`
- `on_llm_start(&str, memory: &SharedMemory)`
- `on_llm_end(&str, response: &Message)`
- `on_llm_error(&str, error: &ProviderError, attempt: usize, will_retry: bool)`
- `before_tool_call(&str, tool_call: &ToolCall) -> Result<(), AgentError>`
- `after_tool_call(&str, tool_call: &ToolCall, observation: &str)`
- `on_tool_failed(&str, tool_call: &ToolCall, error: &str)`

Hooks run in registration order. For async work inside sync hooks (e.g. LLM
summarisation), use `engine::block_on` — a bare `Handle::block_on` panics on
tokio worker threads.

### `AgentEvent` stream
Single `mpsc::unbounded_channel`. Variants:

| Event | When |
| --- | --- |
| `RunStarted { session_id, user_input }` | New task begins |
| `Token(String)` / `ReasoningToken(String)` | LLM output streaming |
| `ToolCallStart { id, name }` | Tool name known before args |
| `ToolCall { id, name, arguments, origin }` | Before tool execution |
| `ToolSuccessful { id, name, output }` | Tool completed |
| `ToolRejected { id, name, reason }` | Hook blocked tool |
| `ToolFailure { id, name, error }` | Tool execution failed |
| `ToolProgress { id, name, message }` | Real-time progress |
| `InterventionRequired(InterventionRequest)` | Hook needs user decision |
| `RunCompleted { answer }` | Success |
| `RunFailed { error }` | Error |
| `Cancelled` | User cancelled |
| `Done` | Sentinel — always last |

`CallOrigin::Llm` vs `CallOrigin::User` distinguishes LLM tool calls from
user-invoked tool calls.

### `AgentBuilder` vs `EngineContextBuilder`
- `Agent::builder(client, model)` — simple API: auto-creates Memory, seeds
  system prompt, collects tools into ToolRegistry.
- `EngineContext::builder(client, memory, tools, model)` — advanced API:
  supply Memory and ToolRegistry explicitly, configure hooks, max_steps,
  max_retries, streaming, pending_hints.

### Two-tier compaction (hooks crate)
1. **MicroCompact** — `on_llm_start()` clears old tool outputs from
   high-volume tools (`read`, `shell`, `grep`, `glob`, `edit`, `write`, `ls`)
   in-place, keeping the most recent N intact (default 10).
2. **MacroCompact** — `on_llm_start()` checks `prompt_tokens` from the last
   `Usage` against a token threshold (default 1,000,000 tokens); when over,
   drains old non-System messages (keeping last N), calls a compact model
   for summarisation via `engine::block_on`, inserts summary as System
   message.

Key constants in `extensions/compact/src/compact.rs`: `DEFAULT_COMPACT_TOKEN_LIMIT`,
`DEFAULT_COMPACT_CHAR_LIMIT`, `DEFAULT_KEEP_LAST_N`, `DEFAULT_KEEP_RECENT_TOOL_OUTPUTS`.

### Sandbox (defense in depth)

| Layer | Component | Role |
| --- | --- | --- |
| 1 | `WorkspaceFs` | Path sandbox — canonicalization, file-size caps, extension blocklist, hidden-file protection, binary detection, TOCTOU re-check; read-only roots are readable but never writable |
| 2 | `ShellFilter` | Command classification — auto-approve, deny patterns, prompt user for rest |
| 3 | `SandboxHook` | Orchestrator — checks quotas, classifies commands, prompts user via `InterventionRequired` + `ResponseRouter`, writes audit log |
| 4 | `EnvSanitizer` | Clears dangerous env vars before spawning child processes |
| 5 | Watchdog | Kills process tree on timeout (`taskkill /F /T` on Windows) |

Config: `SandboxConfig::load(path)` — path provided by the caller; safe
defaults if the file is missing. Shell output is capped at **100 KB**.
Default audit log path: `.agent/audit.jsonl` (relative to workspace root).

### Observability (full-chain tracing)
`ObservabilityHook` captures lifecycle events with timing data and token
counts via a side channel (`Arc<TraceStore>`) shared between the agent task
and the UI. `TraceStore` is a thread-safe ring buffer (4096 entries) with
lock-free `RunMetrics` atomics. Trace events flow via the `tracing` crate.

### Skills system
`SkillRegistry` (extensions/skills) discovers and parses `.md` skill files
(YAML frontmatter + body) from user-configured skill directories. The
registry is provider-agnostic; consuming applications wire it to their own
`SkillTool` / `SkillHook`.

### Persistence
`PersistenceConfig` (defaults: `.agent/threads`, `.agent/current` under the
workspace root) drives thread save/load. `PersistenceHook` auto-saves after
each run; thread names are LLM-generated from the first user query via a
flash model, with a filesystem-safe sanitizer.

### Agent Kit (NVIDIA OO Agents style)
`#[derive(Agent)]` + `#[agent_impl]` macros map the NVIDIA OO Agents
paradigm onto the core API: class doc = system prompt, sync methods =
tools, async methods = LLM generations, Pydantic-style returns = structured
output. See `docs/agent-kit-guide.md`.

### `ResponseRouter`
Maps `request_id` → `SyncSender<InterventionResponse>`. Multiple components
can need user intervention simultaneously — each registers its own channel.
The consuming application routes responses back through the router.
