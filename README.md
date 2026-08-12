# Agent Oxide

[![crates.io](https://img.shields.io/crates/v/agent_oxide.svg)](https://crates.io/crates/agent_oxide)
[![docs.rs](https://docs.rs/agent_oxide/badge.svg)](https://docs.rs/agent_oxide)
[![CI](https://github.com/Nie-Tianyi/agent_oxide/actions/workflows/ci.yml/badge.svg)](https://github.com/Nie-Tianyi/agent_oxide/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

A modular agent framework for Rust — a ReAct loop engine, an LLM provider
abstraction, a tool system, and a rich set of extensions (sandbox,
persistence, skills, observability, subagents).

> **Architecture:** Agent Oxide realizes a four-layer design for the Agenty
> project — top to bottom: **(UI)** TUI / GUI / CLI / WebUI; **(Harness)**
> multi-agent orchestration, skills, plugins; **(Agent core)** memory &
> memory management, the `Tool` trait, the agent loop, hooks; **(LLM client)**
> provider traits — `LLMClient`, `Embedding`, and future `VLM` /
> image-generation traits. This library implements the **bottom two layers**
> (Agent core and LLM client); you provide the **top two** — your own UI
> layer, and the Harness layer (multi-agent orchestration, skills, plugins)
> built on top of the core APIs. A DeepSeek reference client ships with the
> framework.

| Layer | Contents | Implemented by |
| ----- | -------- | -------------- |
| 1 — UI | TUI / GUI / CLI / WebUI | **You** |
| 2 — Harness | Multi-agent orchestration, SKILLs, plugins | **You** |
| 3 — Agent core | Memory & memory management, `Tool` trait, agent loop, hooks | **Agent Oxide** |
| 4 — LLM client | `LLMClient` trait, `Embedding` trait; future: `VLM`, image generation | **Agent Oxide** |

```toml
[dependencies]
agent_oxide = "0.5"
```

## Quick start

```rust,no_run
use agent_oxide::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = DeepSeekClient::new(std::env::var("DEEPSEEK_API")?);

    let agent = Agent::builder(client, "deepseek-chat")
        .system_prompt("You are a helpful assistant.")
        .build();

    let answer = agent.run("What is 2+2?").await?;
    println!("{answer}");
    Ok(())
}
```

## NOOA — NVIDIA OO Agents style (Agent Kit)

> **Flagship ergonomics.** NOOA (NVIDIA OO Agents) lets you program an agent
> as a plain Rust struct: **class doc = system prompt, sync methods = tools,
> async methods = LLM generations, Pydantic-style returns = structured
> output.** It compiles down to the core engine (`Tool` trait, `ToolRegistry`,
> `engine::Agent`) with zero changes to the core engine, and coexists freely
> with the imperative `Agent::builder(...)` API above.

Provided by two crates — `agent-oxide-macros` (`#[derive(Agent)]` +
`#[agent_impl]` + `#[tool]`, compile time) and `agent_oxide` itself (runtime:
`agent_oxide::agent_kit::{AgentBlueprint, Strategy, BuildConfig}`). Add both
to your dependencies and write:

```rust
use agent_oxide::schemars::JsonSchema;
use agent_oxide::serde::{Deserialize, Serialize};
use agent_oxide::{Agent, agent_impl};
use agent_oxide::DeepSeekClient;

/// You are an agent specializing in analyzing customer feedback.   // ← struct doc = system prompt
#[derive(Clone, Agent)]
struct FeedbackAgent {
    #[agent(client)]                          // ← marks the LLM client field
    client: DeepSeekClient,
}

#[agent_impl]
impl FeedbackAgent {
    /// Analyze the sentiment and key topics of customer feedback in one sentence.  // ← method doc = method prompt
    async fn analyze_feedback(&self, text: String) -> String {}  // empty body = LLM generation
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = FeedbackAgent {
        client: DeepSeekClient::new(std::env::var("DEEPSEEK_API")?),
    };

    // Generation method: one-shot call implemented by the LLM.
    let s = agent.analyze_feedback("Great product, but shipping was slow".into()).await?;
    println!("{s}");

    // Or promote to a full core-engine agent (ReAct loop + hook pipeline):
    let engine_agent = agent.into_agent("deepseek-v4-pro")?;
    Ok(())
}
```

Core concepts at a glance:

| Concept | Syntax | Meaning |
| ------- | ------- | ------- |
| System prompt | `/// struct doc comment` | The agent's system prompt |
| Tool | `fn method(&self, ...) { body }` | Sync method = tool; signature auto-derives the JSON Schema |
| Generation | `async fn method(&self, ...) {}` | Empty-body async fn = LLM generation |
| Structured output | `-> T where T: Deserialize + JsonSchema` | Pydantic-style typed returns (auto-retried on parse failure) |
| Strategy | `#[strategy(predict)]` / `#[strategy(code_act, max_iterations = 15)]` | Single LLM call vs full ReAct loop with tools |
| Context | `#[context(static)]` / `#[context(dynamic)]` | Context blocks, re-rendered before every LLM call |
| Full agent | `.into_agent(model)` / `.into_agent_with(model, BuildConfig)` | Promote to `engine::Agent` — all hooks apply (sandbox, persistence, observability, compaction) |

**Rule of thumb:** one-shot tasks that want type-safe returns → **generation
methods**; conversations, security, persistence → **`into_agent()`**. Full
reference: [docs/agent-kit-guide.md](docs/agent-kit-guide.md).

## Crates

Everything ships in **two crates**: `agent_oxide` (all framework code) and
`agent-oxide-macros` (the proc macros `#[derive(Agent)]`, `#[agent_impl]`,
`#[tool]`).

```toml
[dependencies]
agent_oxide = "0.5"
agent-oxide-macros = "0.5"
```

Every feature is a module of `agent_oxide`:

| Module | Role |
| ------ | ---- |
| `provider` | `LLMClient` trait and shared types (`Message`, `ToolCall`, …) |
| `deepseek` | DeepSeek API client |
| `memory` | Conversation memory buffer |
| `tools` | `Tool` trait, `ToolRegistry`, `#[tool]` macro |
| `engine` | Agent ReAct loop, `AgentHook`, `AgentEvent` |
| `util` | Shared utilities (`iso8601_now`) |
| `agent_kit` | NOOA — NVIDIA OO Agents-style ergonomics (`#[derive(Agent)]`) |
| `hooks` | Compaction hooks (micro/macro) |
| `observability` | Full-chain tracing |
| `persistence` | Conversation save/load |
| `sandbox` | 5-layer security sandbox |
| `skills` | Skill discovery & registry |
| `subagent` | Spawn child agents as tools |

## Defining tools

Annotate a struct with `#[tool]` — the macro generates the `Tool` trait
impl and lazily derives a JSON Schema from the args type:

```rust,ignore
use agent_oxide::prelude::*;
use agent_oxide::tools::{Progress, ProgressStream, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    /// The text to echo back.
    pub text: String,
}

#[tool(name = "echo", description = "Echo the input text back unchanged.", args = EchoArgs)]
pub struct EchoTool;

impl EchoTool {
    fn execute_stream(&self, args: EchoArgs) -> Result<ProgressStream, ToolError> {
        Ok(ProgressStream::done(args.text))
    }
}
```

> **Note:** like all proc-macro crates (serde, tokio), `#[tool]` expands to
> code referencing `agent_oxide::tools::` and `agent_oxide::serde_json::`
> paths — so consuming crates need `agent_oxide` as a **direct** dependency
> (a transitive one is not enough).

## Architecture

### Overview

- **Engine** — the ReAct loop runs in a dedicated Tokio task; hooks
  (`AgentHook`) observe or intercept every lifecycle event; streaming
  `AgentEvent`s power real-time UIs.
- **Sandbox** — 5-layer defense: path sandboxing (`WorkspaceFs`), command
  classification (`ShellFilter`), quota orchestration (`SandboxHook`),
  environment sanitization, and a process watchdog.
- **Compaction** — two-tier memory management: micro-compaction clears
  stale tool outputs in place; macro-compaction summarises old messages
  via a cheap model when token usage exceeds a threshold.
- **Persistence** — conversations auto-save as JSON + Markdown, with
  LLM-generated thread titles.
- **Skills** — `.md` files with YAML frontmatter discovered at startup
  and injected as system messages.
- **Agent Kit** — NOOA (NVIDIA OO Agents-style) ergonomics: class-doc =
  system prompt, sync methods = tools, async methods = LLM generations.

Rust 2024 edition, Tokio async. One crate, many modules: the framework
uses Rust 2024 native async fn in traits (RPITIT) — do **NOT** use the
`async-trait` crate. Prefer sync traits for dyn-dispatch; keep async work
in dedicated components.

### Workspace structure

```text
agent_oxide/
├── Cargo.toml              # [package] agent_oxide + [workspace]
├── agent_oxide-macros/     # proc macros: #[derive(Agent)], #[agent_impl], #[tool]
├── src/
│   ├── lib.rs              # module tree + prelude; re-exports core/* and extensions/*
│   ├── core/               # engine layer (layer 3 of the four-layer architecture)
│   │   ├── provider/       # LLMClient trait + shared types
│   │   ├── deepseek/       # DeepSeekClient — implements LLMClient
│   │   ├── tools/          # Tool trait, ToolRegistry, ProgressStream
│   │   ├── memory/         # Memory buffer, PendingHints
│   │   ├── util/           # Shared utilities (iso8601_now)
│   │   └── engine/         # Agent (ReAct loop), AgentHook trait, AgentEvent, ResponseRouter
│   └── extensions/         # optional capabilities layered on the engine
│       ├── skills/         # SkillDef, SkillRegistry — skill discovery & loading
│       ├── hooks/          # MicroCompactHook + MacroCompactHook
│       ├── persistence/    # Conversation persistence — save/load threads, PersistenceHook
│       ├── subagent/       # SubagentTool — spawn child agents as tools
│       ├── observability/  # TraceEvent, TraceStore, RunMetrics — full-chain tracing
│       ├── sandbox/        # Sandbox runtime — WorkspaceFs, ShellFilter, SandboxHook, etc.
│       └── agent_kit/      # NOOA — NVIDIA OO Agents-style ergonomics
├── examples/               # NOOA examples: feedback / inventory / note-taking agents
└── docs/
    ├── beginner-developer-guide.md
    ├── senior-developer-guide.md
    ├── sandbox-architecture.md
    └── agent-kit-guide.md
```

> The public API is flat: every module in `src/core/` and `src/extensions/`
> is re-exported at the crate root, so users write `agent_oxide::provider`,
> `agent_oxide::sandbox`, etc. — the `core/`/`extensions/` split is purely
> an internal organization.

### Module dependency graph

```text
provider (no internal deps)   util (no internal deps)   skills (no internal deps)
    ↑                            ↑
    ├── deepseek ────────────────┘
    ├── tools ─────── (uses provider)
    ├── memory ────── (uses provider)
    ↑
    └── engine ────── (uses provider + tools + memory)
            ↑
hooks ─────────────── (uses provider + memory + engine + util)
persistence ───────── (uses provider + engine + memory + deepseek + util)
observability ─────── (uses provider + engine + memory)
sandbox ───────────── (uses engine + memory + provider + util)
subagent ──────────── (uses provider + tools + engine + memory + observability + util)
agent_kit ─────────── (uses provider + tools + engine + memory)

proc macros (agent_oxide-macros) — generate code against `agent_oxide::` paths
```

### Key patterns

#### `LLMClient` trait

Abstraction over LLM providers. Uses Rust 2024 native async fn (NOT
`#[async_trait]`). `DeepSeekClient` is the reference implementation.
Implement this trait to support a new provider.

#### `Tool` trait

Sync and object-safe. `execute_stream()` returns `ProgressStream` — short
tools emit a single `Progress::Done`, long-running tools (shell) emit
`Progress::InProgress` updates then `Progress::Done`. Use
`tokio::sync::mpsc` from a spawned thread for async I/O.

#### `#[tool]` proc macro

Annotate a struct with `#[tool(name = "...", description = "...", args = ArgsType)]`.
Generates `Tool` trait impl — the struct must define an inherent
`execute_stream(&self, args: ArgsType) -> Result<ProgressStream, ToolError>`.
JSON Schema is lazily generated from `ArgsType` via `schemars`.

> Note: the macro expands to `agent_oxide::tools::` /
> `agent_oxide::serde_json::` paths, so consuming crates need `agent_oxide`
> as a **direct** dependency (standard proc-macro behaviour, like
> serde/tokio).

#### `AgentHook` trait — 9 lifecycle callbacks

All have default no-ops. Naming convention:

| Prefix | Meaning |
| ------ | ------- |
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

#### `AgentEvent` stream

Single `mpsc::unbounded_channel`. Variants:

| Event | When |
| ----- | ---- |
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

#### `AgentBuilder` vs `EngineContextBuilder`

- `Agent::builder(client, model)` — simple API: auto-creates Memory, seeds
  system prompt, collects tools into ToolRegistry.
- `EngineContext::builder(client, memory, tools, model)` — advanced API:
  supply Memory and ToolRegistry explicitly, configure hooks, max_steps,
  max_retries, streaming, pending_hints.

#### Two-tier compaction (hooks crate)

1. **MicroCompact** — `on_llm_start()` clears old tool outputs from
   high-volume tools (`read`, `shell`, `grep`, `glob`, `edit`, `write`, `ls`)
   in-place, keeping the most recent N intact (default 10).
2. **MacroCompact** — `on_llm_start()` checks `prompt_tokens` from the last
   `Usage` against a token threshold (default 1,000,000 tokens); when over,
   drains old non-System messages (keeping last N), calls a compact model
   for summarisation via `engine::block_on`, inserts summary as System
   message.

Key constants in `src/extensions/hooks/compact.rs`: `DEFAULT_COMPACT_TOKEN_LIMIT`,
`DEFAULT_COMPACT_CHAR_LIMIT`, `DEFAULT_KEEP_LAST_N`, `DEFAULT_KEEP_RECENT_TOOL_OUTPUTS`.

#### Sandbox (defense in depth)

| Layer | Component | Role |
| ----- | --------- | ---- |
| 1 | `WorkspaceFs` | Path sandbox — canonicalization, file-size caps, extension blocklist, hidden-file protection, binary detection, TOCTOU re-check; read-only roots are readable but never writable |
| 2 | `ShellFilter` | Command classification — auto-approve, deny patterns, prompt user for rest |
| 3 | `SandboxHook` | Orchestrator — checks quotas, classifies commands, prompts user via `InterventionRequired` + `ResponseRouter`, writes audit log |
| 4 | `EnvSanitizer` | Clears dangerous env vars before spawning child processes |
| 5 | Watchdog | Kills process tree on timeout (`taskkill /F /T` on Windows) |

Config: `SandboxConfig::load(path)` — path provided by the caller; safe
defaults if the file is missing. Shell output is capped at **100 KB**.
Default audit log path: `.agent/audit.jsonl` (relative to workspace root).

#### Observability (full-chain tracing)

`ObservabilityHook` captures lifecycle events with timing data and token
counts via a side channel (`Arc<TraceStore>`) shared between the agent task
and the UI. `TraceStore` is a thread-safe ring buffer (4096 entries) with
lock-free `RunMetrics` atomics. Trace events flow via the `tracing` crate.

#### Skills system

`SkillRegistry` (src/extensions/skills) discovers and parses `.md` skill files
(YAML frontmatter + body) from user-configured skill directories. The
registry is provider-agnostic; consuming applications wire it to their own
`SkillTool` / `SkillHook`.

#### Persistence

`PersistenceConfig` (defaults: `.agent/threads`, `.agent/current` under the
workspace root) drives thread save/load. `PersistenceHook` auto-saves after
each run; thread names are LLM-generated from the first user query via a
flash model, with a filesystem-safe sanitizer.

#### Agent Kit (NVIDIA OO Agents style)

`#[derive(Agent)]` + `#[agent_impl]` macros map the NOOA (NVIDIA OO Agents)
paradigm onto the core API: class doc = system prompt, sync methods =
tools, async methods = LLM generations, Pydantic-style returns = structured
output. See the NOOA section above and `docs/agent-kit-guide.md`.

#### `ResponseRouter`

Maps `request_id` → `SyncSender<InterventionResponse>`. Multiple components
can need user intervention simultaneously — each registers its own channel.
The consuming application routes responses back through the router.

### Build & Test

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

## Environment

| Var | Purpose |
| --- | ------- |
| `DEEPSEEK_API` | DeepSeek API key (required by the `deepseek` client) |
| `BASE_URL` | API base URL (default `https://api.deepseek.com`) |

## Documentation

| Doc | For |
| --- | --- |
| [beginner-developer-guide.md](docs/beginner-developer-guide.md) | Build your first AI agent in 10 minutes — no prior agent experience needed |
| [senior-developer-guide.md](docs/senior-developer-guide.md) | In-depth reference for experienced Rust developers — internals, trait implementations, advanced patterns, design decisions |
| [sandbox-architecture.md](docs/sandbox-architecture.md) | Sandbox architecture — the full security check chain of an LLM tool call |
| [agent-kit-guide.md](docs/agent-kit-guide.md) | NOOA full reference — macros, strategies, structured output, `into_agent` |
| [nooa-agent-guide.md](docs/nooa-agent-guide.md) | Step-by-step tutorial — define your first NOOA agent, one concept at a time |
| [harness-ui-guide.md](docs/harness-ui-guide.md) | Build your own Harness (orchestration, SKILLs, plugins) and UI layers with the core APIs |

## License

MIT OR Apache-2.0
