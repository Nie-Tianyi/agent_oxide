# Agent Oxide

A modular agent framework for Rust — a ReAct loop engine, an LLM provider
abstraction, a tool system, and a rich set of extensions (sandbox,
persistence, skills, observability, subagents).

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

## Crates

The umbrella crate re-exports every sub-crate, so `use agent_oxide::...`
gives you the whole framework:

| Module | Crate | Role |
|--------|-------|------|
| `agent_oxide::provider` | `provider` | `LLMClient` trait and shared types (`Message`, `ToolCall`, …) |
| `agent_oxide::deepseek` | `deepseek` | DeepSeek API client |
| `agent_oxide::memory` | `memory` | Conversation memory buffer |
| `agent_oxide::tools` | `tools` | `Tool` trait, `ToolRegistry`, `#[tool]` macro |
| `agent_oxide::engine` | `engine` | Agent ReAct loop, `AgentHook`, `AgentEvent` |
| `agent_oxide::util` | `util` | Shared utilities |
| `agent_oxide::agent_kit` | `agent-kit` | NVIDIA OO Agents-style ergonomics (`#[derive(Agent)]`) |
| `agent_oxide::hooks` | `hooks` | Compaction hooks (micro/macro) |
| `agent_oxide::observability` | `observability` | Full-chain tracing |
| `agent_oxide::persistence` | `persistence` | Conversation save/load |
| `agent_oxide::sandbox` | `sandbox` | 5-layer security sandbox |
| `agent_oxide::skills` | `skills` | Skill discovery & registry |
| `agent_oxide::subagent` | `subagent` | Spawn child agents as tools |

Each sub-crate is also published independently on crates.io — depend on
just the pieces you need:

```toml
[dependencies]
agent_oxide = "0.5"          # everything
engine = "0.5"               # just the ReAct loop
sandbox = "0.5"              # just the sandbox
```

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
> code referencing `::tools::` and `::serde_json::` paths, so consuming
> crates must add `tools` and `serde_json` as direct dependencies.

## Architecture

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
- **Agent Kit** — NVIDIA OO Agents-style ergonomics: class-doc = system
  prompt, sync methods = tools, async methods = LLM generations.

See `docs/` for the senior developer guide, beginner guide, sandbox
architecture, and agent-kit guide.

## Environment

| Var | Purpose |
|-----|---------|
| `DEEPSEEK_API` | DeepSeek API key (required by the `deepseek` client) |
| `BASE_URL` | API base URL (default `https://api.deepseek.com`) |

## License

MIT OR Apache-2.0
