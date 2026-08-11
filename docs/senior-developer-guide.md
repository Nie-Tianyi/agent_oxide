# Loomis Senior Developer Guide

> **In-depth reference for experienced Rust developers.**  
> Covers architecture internals, trait implementations, advanced patterns, and design decisions.

---

## Table of Contents

1. [Architecture Deep Dive](#1-architecture-deep-dive)
2. [LLM Providers — The `LLMClient` Trait](#2-llm-providers--the-llmclient-trait)
3. [Tools In Depth — The `Tool` Trait & `ProgressStream`](#3-tools-in-depth--the-tool-trait--progressstream)
4. [AgentHook — The Complete Lifecycle](#4-agenthook--the-complete-lifecycle)
5. [Advanced Assembly — `EngineContext` & Full Wiring](#5-advanced-assembly--enginecontext--full-wiring)
6. [Memory & Two-Tier Compaction](#6-memory--two-tier-compaction)
7. [Event System & TUI Architecture](#7-event-system--tui-architecture)
8. [WorkspaceFs Sandbox Internals](#8-workspacefs-sandbox-internals)
9. [Multi-Layer Sandbox Defense](#9-multi-layer-sandbox-defense)
10. [Subagent System](#10-subagent-system)
11. [Conversation Persistence](#11-conversation-persistence)
12. [Complete Reference Example — Code Reviewer](#12-complete-reference-example--code-reviewer)
13. [Appendices](#13-appendices)

---

## 1. Architecture Deep Dive

### Workspace Structure

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["core/*", "extensions/*", "bins/*"]
├── core/
│   ├── provider/           # LLMClient trait + shared types
│   ├── deepseek/           # DeepSeekClient — implements LLMClient
│   ├── tools/              # Tool trait, ToolRegistry
│   ├── tools-macros/       # #[tool] proc macro — generates Tool trait impls
│   ├── memory/             # Memory (Vec<Message> buffer)
│   ├── util/               # Shared workspace utilities (iso8601_now)
│   └── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent
├── extensions/
│   ├── hooks/              # MicroCompactHook, MacroCompactHook
│   ├── persistence/        # Save/load threads, thread naming, PersistenceHook
│   ├── observability/      # TraceEvent, TraceStore, RunMetrics
│   ├── sandbox/            # WorkspaceFs, ShellFilter, SandboxHook, etc.
│   ├── skills/             # SkillDef, SkillRegistry, ActiveSkills
│   └── subagent/           # SubagentTool — spawn child agents as tools
└── bins/
    └── loomis/             # Binary — tools, sandbox, hooks, TUI, main.rs
```

### Dependency Graph (no cycles)

```
provider ────────────────────────── (no internal deps)
    ↑
    ├── deepseek ──────────────── (implements LLMClient)
    ├── tools ─────────────────── (uses ToolDef from provider)
    ├── memory ────────────────── (uses Message from provider)
    ↑
    ├── engine ────────────────── (uses provider + tools + memory)
    │       ↑
    ├── hooks ─────────────────── (uses provider + memory + engine)
    │       ↑
    ├── persistence ───────────── (uses provider + memory + engine)
    │       ↑
    ├── subagent ──────────────── (uses provider + tools + engine + memory)
    │       ↑
    └── loomis (bin) ──────────── (uses everything)
```

### Core Abstractions Table

| Crate | Key Type | Role | Location |
|---|---|---|---|
| `provider` | `LLMClient` | Abstraction over LLM APIs | `core/provider/src/lib.rs` |
| `provider` | `Message`, `Role`, `ToolCall`, `ToolDef` | Conversation primitives | `core/provider/src/types.rs` |
| `provider` | `CompletionRequest`, `CompletionResponse` | API request/response types | `core/provider/src/types.rs` |
| `provider` | `StreamChunk`, `Delta` | SSE streaming primitives | `core/provider/src/types.rs` |
| `deepseek` | `DeepSeekClient` | Concrete LLMClient impl | `core/deepseek/src/lib.rs` |
| `tools` | `Tool` | Trait — sync, Send+Sync, object-safe | `core/tools/src/tool.rs` |
| `tools` | `ToolRegistry` | Name-indexed tool collection | `core/tools/src/registry.rs` |
| `tools` | `Progress`, `ProgressStream` | Tool output streaming | `core/tools/src/progress.rs` |
| `sandbox` | `WorkspaceFs` | Sandboxed filesystem | `extensions/sandbox/src/fs.rs` |
| `sandbox` | `SandboxConfig` | Security policy (TOML) | `extensions/sandbox/src/config.rs` |
| `tools-macros` | `#[tool]` | Proc macro codegen | `core/tools-macros/src/lib.rs` |
| `memory` | `Memory`, `SharedMemory` | Conversation buffer | `core/memory/src/memory.rs` |
| `persistence` | `PersistenceConfig` | Thread storage layout | `extensions/persistence/src/persistence.rs` |
| `persistence` | `PersistenceHook` | Auto-save after each run | `extensions/persistence/src/hook.rs` |
| `hooks` | `MicroCompactHook` | Tool-output clearing | `extensions/compact/src/compact.rs` |
| `hooks` | `MacroCompactHook<C>` | LLM summarisation | `extensions/compact/src/compact.rs` |
| `engine` | `Agent` | ReAct loop runner | `core/engine/src/agent.rs` |
| `engine` | `AgentEvent` | Unified event stream (single channel) | `core/engine/src/events.rs` |
| `engine` | `AgentHook` | 10-callback lifecycle trait | `core/engine/src/hooks.rs` |
| `engine` | `EngineContext` | Agent dependencies (advanced API) | `core/engine/src/context.rs` |
| `engine` | `InterveneRequest`, `InterveneResponse` | User-interaction protocol | `core/engine/src/events.rs` |
| `subagent` | `SubagentTool<C>` | Child agent as a Tool | `extensions/subagent/src/tool.rs` |
| `subagent` | `SubagentConfig` | Child agent policy | `extensions/subagent/src/config.rs` |

### Design Decisions

**Why sync `Tool` trait?**
Async fn in traits became stable in Rust 2024, but they're not object-safe for
`dyn Tool` dispatch.  `Tool` is `Send + Sync` and returns `ProgressStream` — a
synchronous `mpsc` bridge that carries async computation results.  Long-running
tools (like shell execution) spawn a thread and send updates through the channel.
This keeps the trait object-safe without `async_trait`.

**Why a single event channel?**
The `Agent` uses one `mpsc::unbounded_channel::<AgentEvent>` for both LLM
streaming tokens and tool execution events.  This simplifies TUI consumers —
one channel, one match arm.  No separate "hook events" vs "streaming events"
channels to merge.

**Why `Agent::builder()` AND `EngineContext::builder()`?**
`Agent::builder(client, model)` is the simple path — it auto-creates
`SharedMemory`, auto-pushes system prompt, and wraps tools in a `ToolRegistry`.
`EngineContext::builder(client, memory, tools, model)` is the advanced path for
when you need control over the Memory lifecycle (pre-loading conversations,
shared memory across agents, custom compaction setup).

---

## 2. LLM Providers — The `LLMClient` Trait

### Trait Definition

```rust
// core/provider/src/lib.rs
use crate::CompletionRequest;
use crate::CompletionResponse;
use crate::ProviderError;
use crate::StreamChunk;
use futures_util::Stream;

pub trait LLMClient: Send + Sync {
    /// Generate a completion from the given request.
    fn generate(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, ProviderError>> + Send;

    /// Stream a completion, yielding chunks as they arrive.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<impl Stream<Item = Result<StreamChunk, ProviderError>> + Send, ProviderError>> + Send;
}
```

This is Rust 2024 native `async fn` in traits (RPITIT) — **no `#[async_trait]` needed**.

### Key Types

```rust
// core/provider/src/types.rs

pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<ToolDef>>,
    pub tool_choice: Option<ToolChoice>,
    pub stream: bool,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
}

pub struct CompletionResponse {
    pub message: Message,
    pub usage: Option<Usage>,
}

pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,          // For tool-result messages
}

pub enum Role { System, User, Assistant, Tool }

pub struct ToolCall {
    pub id: String,
    pub function_name: String,
    pub arguments: String,             // JSON string
}

pub struct ToolDef {
    pub r#type: String,                // "function"
    pub function: FunctionDef,
}

pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
}

// SSE streaming primitives
pub struct StreamChunk {
    pub delta: Option<Delta>,
}

pub struct Delta {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}
```

### Implementing a New Provider (e.g., Anthropic)

**Step 1: Understand the mapping.**

Your job is to convert between Loomis's `CompletionRequest` / `CompletionResponse`
and your provider's native API format.  The key translation points:

| Loomis Type | → | Your Provider |
|---|---|---|
| `CompletionRequest.messages` | → | Provider's chat messages array |
| `CompletionRequest.tools` | → | Provider's tools/functions list |
| `CompletionRequest.stream` | → | Provider's `stream: true/false` |
| Provider's response text | → | `CompletionResponse.message.content` |
| Provider's tool calls | → | `CompletionResponse.message.tool_calls` |
| Provider's SSE events | → | `StreamChunk` with `Delta` |

**Step 2: Skeleton implementation.**

```rust
use provider::{
    CompletionRequest, CompletionResponse, LLMClient, Message, ProviderError,
    Role, StreamChunk, Delta, ToolCall, Usage,
};
use futures_util::Stream;
use std::pin::Pin;

pub struct AnthropicClient {
    api_key: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| "ANTHROPIC_API_KEY not set")?;
        Ok(Self {
            api_key,
            http: reqwest::Client::new(),
        })
    }

    /// Convert Loomis messages to Anthropic's format.
    fn convert_messages(&self, messages: &[Message]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .map(|msg| {
                serde_json::json!({
                    "role": match msg.role {
                        Role::System => "system",
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::Tool => "user", // Anthropic: tool results are user messages
                    },
                    "content": msg.content,
                })
            })
            .collect()
    }

    /// Convert Loomis ToolDefs to Anthropic's tool format.
    fn convert_tools(&self, tools: &[provider::ToolDef]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|td| {
                serde_json::json!({
                    "name": td.function.name,
                    "description": td.function.description,
                    "input_schema": td.function.parameters, // JSON Schema
                })
            })
            .collect()
    }

    /// Convert Anthropic's response to Loomis's CompletionResponse.
    fn convert_response(&self, body: serde_json::Value) -> Result<CompletionResponse, ProviderError> {
        let content_block = &body["content"][0];
        let mut content = String::new();
        let mut tool_calls = Vec::new();

        match content_block["type"].as_str() {
            Some("text") => {
                content = content_block["text"].as_str()
                    .unwrap_or("")
                    .to_string();
            }
            Some("tool_use") => {
                let tc = ToolCall {
                    id: content_block["id"].as_str().unwrap_or("").to_string(),
                    function_name: content_block["name"].as_str().unwrap_or("").to_string(),
                    arguments: content_block["input"].to_string(),
                };
                tool_calls.push(tc);
            }
            _ => {}
        }

        Ok(CompletionResponse {
            message: Message {
                role: Role::Assistant,
                content,
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                tool_call_id: None,
                name: None,
            },
            usage: None, // Parse from body if available
        })
    }

    /// Convert Anthropic's SSE event to Loomis's StreamChunk.
    fn convert_sse_event(&self, event: serde_json::Value) -> Result<Option<StreamChunk>, ProviderError> {
        match event["type"].as_str() {
            Some("content_block_delta") => {
                let delta_type = event["delta"]["type"].as_str();
                match delta_type {
                    Some("text_delta") => {
                        Ok(Some(StreamChunk {
                            delta: Some(Delta {
                                content: Some(event["delta"]["text"].as_str().unwrap_or("").to_string()),
                                reasoning_content: None,
                                tool_calls: None,
                            }),
                        }))
                    }
                    Some("input_json_delta") => {
                        Ok(Some(StreamChunk {
                            delta: Some(Delta {
                                content: None,
                                reasoning_content: None,
                                tool_calls: Some(vec![ToolCall {
                                    id: String::new(),
                                    function_name: String::new(),
                                    arguments: event["delta"]["partial_json"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string(),
                                }]),
                            }),
                        }))
                    }
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }
}

impl LLMClient for AnthropicClient {
    async fn generate(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let body = serde_json::json!({
            "model": request.model,
            "messages": self.convert_messages(&request.messages),
            "tools": request.tools.as_ref().map(|t| self.convert_tools(t)),
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "stream": false,
        });

        let resp = self.http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Http(format!(
                "Anthropic API returned {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Deserialization(e.to_string()))?;

        self.convert_response(body)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<impl Stream<Item = Result<StreamChunk, ProviderError>> + Send, ProviderError> {
        // Build request with stream:true, parse SSE bytes line by line,
        // convert each event through convert_sse_event().
        // See DeepSeekClient for a complete SSE streaming example.
        todo!("Implement SSE streaming")
    }
}
```

### Error Handling

```rust
// core/provider/src/lib.rs
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    #[error("Stream error: {0}")]
    Stream(String),

    #[error("Rate limited — retry after {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },

    #[error("Authentication failed")]
    Authentication,

    #[error("Provider returned no response")]
    NoResponse,
}
```

The engine retries on `ProviderError::RateLimited` and transient HTTP errors
(`5xx`), up to `max_retries` times (default: 3).

### SSE Streaming Pipeline (DeepSeek Reference)

```
HTTP chunk → buffer
    → find_event_end ("\n\n")
    → extract_sse_data (strip "data: " prefix, skip "[DONE]")
    → parse JSON → StreamChunk
    → yield to Agent
```

Key implementation detail: buffering is byte-level because SSE events can be
split across TCP frames.  The buffer accumulates until `\n\n` is found, then
the complete event is parsed.

---

## 3. Tools In Depth — The `Tool` Trait & `ProgressStream`

### The `Tool` Trait (manual implementation)

While `#[tool]` covers 95% of cases, manual implementation gives you full control.
The trait is sync and object-safe:

```rust
// core/tools/src/tool.rs
use serde_json::Value;
use crate::{ProgressStream, ToolError};

pub trait Tool: Send + Sync {
    /// Unique name used in tool_call requests and registry lookup.
    fn name(&self) -> &str;

    /// Human-readable description. Passed to the LLM as part of the ToolDef.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's arguments. Passed to the LLM as part of the ToolDef.
    fn parameter_schema(&self) -> Value;

    /// Execute the tool with a JSON arguments string. Returns a ProgressStream.
    fn execute_stream(&self, args: &str) -> Result<ProgressStream, ToolError>;

    /// Convenience: build a ToolDef from the trait methods.
    fn to_def(&self) -> provider::ToolDef {
        provider::ToolDef {
            r#type: "function".into(),
            function: provider::FunctionDef {
                name: self.name().into(),
                description: self.description().into(),
                parameters: self.parameter_schema(),
            },
        }
    }
}
```

### `#[tool]` Macro — What It Generates

Given:

```rust
#[tool(name = "my_tool", description = "Does X.", args = MyArgs)]
struct MyTool { /* fields */ }

impl MyTool {
    fn execute_stream(&self, args: MyArgs) -> Result<ProgressStream, ToolError> {
        // ...
    }
}
```

The macro generates:

```rust
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Does X." }

    fn parameter_schema(&self) -> Value {
        // Auto-generated JSON Schema from MyArgs (via schemars)
        schemars::schema_for!(MyArgs)
    }

    fn execute_stream(&self, args: &str) -> Result<ProgressStream, ToolError> {
        // Deserialize JSON string into MyArgs, then delegate
        let args: MyArgs = serde_json::from_str(args)
            .map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        MyTool::execute_stream(self, args)
    }
}
```

> **Requirement:** Your args struct must derive `JsonSchema` and `Deserialize`
> with `#[serde(deny_unknown_fields)]`.

### `ProgressStream` Internals

```rust
// core/tools/src/progress.rs

pub enum Progress {
    /// Non-terminal update from a long-running tool.
    InProgress(String),
    /// Terminal — tool finished. Contains the final output.
    Done(String),
}

/// A stream of Progress events. Tools return this from execute_stream().
/// The engine pulls events and emits AgentEvent::ToolProgress for each
/// InProgress, then ToolSuccessful on Done.
pub struct ProgressStream { /* internal mpsc::Receiver<Progress> */ }
```

For simple tools (synchronous, instant result):

```rust
fn execute_stream(&self, args: MyArgs) -> Result<ProgressStream, ToolError> {
    let result = do_work(args)?;
    Ok(ProgressStream::done(result))
}
```

For long-running tools (shell, subagent):

```rust
fn execute_stream(&self, args: MyArgs) -> Result<ProgressStream, ToolError> {
    let (tx, rx) = tokio::sync::mpsc::channel(32);

    std::thread::spawn(move || {
        // Send progress updates as the work happens
        tx.blocking_send(Progress::InProgress("Starting...".into())).ok();

        for i in 0..10 {
            tx.blocking_send(Progress::InProgress(format!("Step {i}..."))).ok();
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        tx.blocking_send(Progress::Done("All steps complete!".into())).ok();
    });

    Ok(ProgressStream::from_receiver(rx))
}
```

> **Why a thread?** The `Tool` trait is sync — it can't hold `.await`.  Spawning
> a thread and communicating through `tokio::sync::mpsc` bridges sync and async
> worlds.  The engine's async runtime polls the receiver.

### `ToolError`

```rust
// core/tools/src/lib.rs
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("Filesystem error: {0}")]
    Fs(String),

    #[error("JSON deserialization error: {0}")]
    Deserialization(String),

    #[error("Tool execution failed: {0}")]
    Execution(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),
}
```

Map each error variant carefully — the engine uses the variant to decide
which `AgentEvent` to emit (`ToolFailure` for execution errors, `ToolRejected`
for permission denials).

### `ToolRegistry`

```rust
use tools::ToolRegistry;
use std::sync::Arc;

let mut registry = ToolRegistry::new();

// Register a tool (Arc<Tool>)
registry.register(Arc::new(MyTool::new()));

// Look up by name
if let Some(tool) = registry.get("my_tool") {
    let stream = tool.execute_stream("{\"key\": \"value\"}")?;
}

// Generate ToolDefs for API requests
let defs = registry.to_tool_defs();

// Iterate
for (name, tool) in registry.iter() {
    println!("Tool: {}", name);
}
```

The registry is `Send + Sync` — tools inside are `Arc<dyn Tool>`, which you can
share across agents (e.g., parent agent and subagents).

---

## 4. AgentHook — The Complete Lifecycle

### Full Trait Definition

```rust
// core/engine/src/hooks.rs
use memory::SharedMemory;
use provider::{Message, ProviderError};
use crate::{CallOrigin, InterveneRequest, InterveneResponse, RunOutcome};

pub trait AgentHook: Send + Sync {
    /// Called once when Agent::run() starts. session_id is a UUID.
    fn on_run_start(&self, session_id: &str) {}

    /// Called once when the run finishes (success, error, or cancellation).
    fn on_run_finish(&self, session_id: &str, outcome: &RunOutcome) {}

    /// Called at the start of each ReAct loop iteration.
    fn on_step_start(&self, session_id: &str, step: usize, max: usize) {}

    /// Called before the LLM call. Receives shared memory for inspection
    /// or modification. This is where MicroCompact and MacroCompact run.
    fn on_llm_start(&self, session_id: &str, memory: &SharedMemory) {}

    /// Called after a successful LLM response.
    fn on_llm_end(&self, session_id: &str, response: &Message) {}

    /// Called when an LLM call fails. `will_retry` is true if the engine
    /// will attempt again.
    fn on_llm_error(
        &self,
        session_id: &str,
        error: &ProviderError,
        attempt: usize,
        will_retry: bool,
    ) {}

    /// Called BEFORE tool execution. Return Err(reason) to block the tool.
    /// This is how SandboxHook enforces security — check permissions, quotas,
    /// and user approval here.
    fn before_tool_call(
        &self,
        session_id: &str,
        name: &str,
        arguments: &str,
        origin: CallOrigin,
    ) -> Result<(), String> {
        Ok(()) // Allow by default
    }

    /// Called AFTER successful tool execution.
    fn after_tool_call(
        &self,
        session_id: &str,
        name: &str,
        output: &str,
        origin: CallOrigin,
    ) {}

    /// Called when a tool execution fails.
    fn on_tool_failed(
        &self,
        session_id: &str,
        name: &str,
        error: &str,
        origin: CallOrigin,
    ) {}
}
```

### Callback Execution Order

```
agent.run(user_input)
  │
  ├─ on_run_start(session_id)
  │
  ├─ [ReAct Loop]
  │     │
  │     ├─ on_step_start(session_id, step_num, max_steps)
  │     │
  │     ├─ on_llm_start(session_id, &memory)     ← Compaction happens here
  │     │
  │     ├─ [LLM API call]
  │     │     │
  │     │     ├─ success → on_llm_end(session_id, &response)
  │     │     │
  │     │     └─ failure → on_llm_error(session_id, &error, attempt, will_retry)
  │     │
  │     ├─ [For each tool_call]
  │     │     │
  │     │     ├─ before_tool_call(…)  ← Can return Err() to block
  │     │     │
  │     │     ├─ [Tool execution]
  │     │     │     ├─ success → after_tool_call(…)
  │     │     │     └─ failure → on_tool_failed(…)
  │     │
  │     └─ [Loop or exit]
  │
  └─ on_run_finish(session_id, &outcome)
```

### `before_tool_call` — The Interception Point

This is the most powerful hook.  You can:

1. **Block dangerous calls**: return `Err("Access denied")`
2. **Ask the user for approval**: emit `NeedUserIntervene` event
3. **Check resource quotas**: deny if exceeded
4. **Log all operations**: record every tool call

### `InterveneRequest` / `InterveneResponse` — User Approval Flow

For interactive user prompts (e.g., "Allow shell command: rm -rf docs/?"),
the SandboxHook uses a rendez-vous channel:

```rust
// In before_tool_call:
let (tx, rx) = std::sync::mpsc::sync_channel::<InterveneResponse>(1);

// Send the request through the event system
event_sender.send(AgentEvent::NeedUserIntervene(InterveneRequest {
    message: "Shell command: rm -rf docs/ — Proceed?".into(),
    options: vec!["Approve".into(), "Deny".into()],
    responder: tx,
})).ok();

// BLOCK until the user responds (rendez-vous channel)
let response = rx.recv().map_err(|_| "User disconnected".to_string())?;

match response.chosen {
    Some("Approve") => Ok(()),
    _ => Err("User denied".to_string()),
}
```

The TUI (or any consumer) receives `AgentEvent::NeedUserIntervene`, renders the
options, captures user input, and sends back the `InterveneResponse`.

### Hook Ordering

Hooks are called in registration order.  If hook A returns `Err` from
`before_tool_call`, hooks B, C, … are **not called** for that tool call.

```rust
Agent::builder(client, model)
    .hook(LoggingHook)       // Called 1st
    .hook(MetricHook)        // Called 2nd
    .hook(SandboxHook)       // Called 3rd (last line of defense)
    .build();
```

### Complete Hook Example — Approval Hook

```rust
use engine::{AgentEvent, AgentHook, CallOrigin, InterveneRequest, InterveneResponse};
use memory::SharedMemory;
use provider::Message;
use std::sync::mpsc::SyncSender;
use tokio::sync::mpsc::UnboundedSender;

pub struct ApprovalHook {
    event_tx: UnboundedSender<AgentEvent>,
    approved_commands: std::sync::Mutex<Vec<String>>,
}

impl ApprovalHook {
    pub fn new(event_tx: UnboundedSender<AgentEvent>) -> Self {
        Self {
            event_tx,
            approved_commands: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl AgentHook for ApprovalHook {
    fn before_tool_call(
        &self,
        _session_id: &str,
        name: &str,
        arguments: &str,
        _origin: CallOrigin,
    ) -> Result<(), String> {
        // Auto-approve safe tools
        if matches!(name, "read" | "grep" | "glob" | "calculator") {
            return Ok(());
        }

        // Shell and write tools require approval
        let (tx, rx) = std::sync::mpsc::sync_channel(1);

        self.event_tx.send(AgentEvent::NeedUserIntervene(InterveneRequest {
            message: format!("Tool: {name}\nArgs: {arguments}\n\nApprove?"),
            options: vec!["Approve".into(), "Deny".into()],
            responder: tx,
        })).map_err(|_| "Event channel closed".to_string())?;

        let response = rx.recv().map_err(|_| "User disconnected".to_string())?;

        match response.chosen.as_deref() {
            Some("Approve") => Ok(()),
            _ => Err("User denied".to_string()),
        }
    }
}
```

---

## 5. Advanced Assembly — `EngineContext` & Full Wiring

### When to Use `EngineContext`

Use `EngineContext::builder()` instead of `Agent::builder()` when you need:

1. **Custom Memory** — pre-load conversation history, share memory across agents
2. **Custom ToolRegistry** — share tools between parent and subagent
3. **More hook control** — register hooks on EngineContext directly
4. **Pending hints** — inject user messages during running agent

### Full Wiring Example

```rust
use deepseek::DeepSeekClient;
use engine::{Agent, EngineContext};
use hooks::{MicroCompactHook, MacroCompactHook};
use memory::{Memory, SharedMemory};
use std::sync::Arc;
use tools::ToolRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = DeepSeekClient::from_env()?;

    // ── 1. Create shared memory (pre-loaded) ───────────────────────────
    let mut memory = Memory::new();
    memory.push(provider::Message::new(
        provider::Role::System,
        "You are an expert Rust developer...",
    ));
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(memory));

    // ── 2. Build tool registry ─────────────────────────────────────────
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MyTool::new()));
    registry.register(Arc::new(AnotherTool::new()));
    let registry = Arc::new(registry);

    // ── 3. Create compaction hooks ─────────────────────────────────────
    let micro = MicroCompactHook::default();
    let macro_compact = MacroCompactHook::new(
        client.clone(),
        "deepseek-v4-flash", // cheaper model for summarisation
    );

    // ── 4. Build EngineContext ─────────────────────────────────────────
    let ctx = EngineContext::builder(client, memory, registry, "deepseek-chat")
        .hook(micro)
        .hook(macro_compact)
        .max_steps(100)
        .max_retries(5)
        .streaming(true)
        .build();

    // ── 5. Create Agent from context ───────────────────────────────────
    let agent = Agent::new(ctx);

    // ── 6. Run ─────────────────────────────────────────────────────────
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let handle = tokio::spawn(async move {
        agent.run_with_events("Refactor the error handling in src/parser.rs", tx).await
    });

    while let Some(event) = rx.recv().await {
        // Handle events...
    }

    let result = handle.await??;
    Ok(())
}
```

### How `Agent::builder()` Maps to `EngineContext`

```rust
// Agent::builder(client, "deepseek-chat")
//     .system_prompt("Be helpful.")
//     .tool(MyTool)
//     .hook(MyHook)
//     .build()
//
// Is equivalent to:
let memory = SharedMemory::default();
// System prompt is pushed into memory automatically

let mut registry = ToolRegistry::new();
registry.register(Arc::new(MyTool));
let registry = Arc::new(registry);

let ctx = EngineContext::builder(client, memory.clone(), registry, "deepseek-chat")
    .hook(MyHook)
    .build();

let agent = Agent::new(ctx);
```

The two APIs are interchangeable — choose `Agent::builder()` for simplicity,
`EngineContext::builder()` for control.

---

## 6. Memory & Two-Tier Compaction

### Memory Internals

```rust
// core/memory/src/memory.rs
pub struct Memory {
    pub messages: Vec<Message>,
    pub last_usage: Option<Usage>,
}

pub type SharedMemory = Arc<RwLock<Memory>>;

impl Memory {
    pub fn push(&mut self, message: Message) { ... }
    pub fn messages(&self) -> &[Message] { ... }
    pub fn to_context_vec(&self) -> Vec<Message> { ... }
    pub fn total_chars(&self) -> usize {
        self.messages.iter().map(|m| m.content.len()).sum()
    }
}
```

`SharedMemory` is an `Arc<RwLock<Memory>>` — multiple components can read/write:
- **Agent**: pushes user messages, LLM responses, tool results
- **Hooks**: modifies memory in-place (compaction)
- **Subagent**: reads parent memory for context inheritance

### MicroCompaction — High-Volume Tool Output Clearing

**Problem:** Tools like `read`, `grep`, `shell`, `glob`, `edit`, `write`, `ls`
can produce large outputs.  Over many steps, the conversation context grows
rapidly, exhausting the LLM's context window.

**Solution:** `MicroCompactHook` in `on_llm_start()` scans all messages.
For high-volume tool results (identified via `tool_call_id` → tool-name
mapping), it replaces the content in-place with a contextual placeholder
(e.g. `[Cleared: read src/fs.rs:10-59]`, parsed from the tool-call
arguments; falls back to `[Old tool result content cleared]` when
unparseable) — the messages remain in the conversation (tool call ID
pairing is preserved), but the content is pruned.

```rust
// extensions/compact/src/compact.rs
pub struct MicroCompactHook {
    high_volume_tools: Vec<String>,
    keep_most_recent: usize,
}

impl Default for MicroCompactHook {
    fn default() -> Self {
        Self {
            high_volume_tools: vec![
                "read".into(), "grep".into(), "shell".into(), "glob".into(),
                "edit".into(), "write".into(), "ls".into(),
            ],
            keep_most_recent: 5,
        }
    }
}

impl AgentHook for MicroCompactHook {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let mut mem = memory.write().unwrap();
        let total_msgs = mem.messages.len();
        let keep = self.keep_most_recent.min(total_msgs);

        for i in 0..(total_msgs - keep) {
            let msg = &mut mem.messages[i];
            if msg.role == Role::Tool {
                if let Some(name) = &msg.name {
                    if self.high_volume_tools.contains(name) {
                        msg.content = "[Old tool result content cleared]".into();
                    }
                }
            }
        }
    }
}
```

**Configuration:**
```toml
# Not configurable via CLI — edit in code or use defaults.
# The default keeps the most recent 5 tool results intact.
```

### MacroCompaction — LLM Summarisation

**Problem:** When the conversation exceeds a size threshold (default: ~2M chars),
even MicroCompaction isn't enough.  We need to summarise the middle of the
conversation.

**Solution:** `MacroCompactHook<C>` in `on_llm_start()` checks
`memory.total_chars()` against a threshold.  If exceeded:

1. Drains all non-System messages from Memory (mutating in-place)
2. Keeps the most recent N messages (default: 10)
3. Calls a compact model (cheaper/faster) with a summarisation prompt
4. Inserts the summary as a System message
5. The agent continues with a drastically smaller context

```rust
// extensions/compact/src/compact.rs
pub const DEFAULT_COMPACT_CHARS: usize = 2_000_000;  // ~2M chars
pub const DEFAULT_KEEP_LAST_N: usize = 10;

pub struct MacroCompactHook<C: LLMClient> {
    client: C,
    model: String,
    max_chars: usize,
    keep_last_n: usize,
}

impl<C: LLMClient> MacroCompactHook<C> {
    pub fn new(client: C, model: &str) -> Self {
        Self {
            client,
            model: model.into(),
            max_chars: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }
}

impl<C: LLMClient> AgentHook for MacroCompactHook<C> {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let total_chars = {
            let mem = memory.read().unwrap();
            mem.total_chars()
        };

        if total_chars < self.max_chars {
            return; // Nothing to do
        }

        // 1. Take all non-System messages except the last N
        let (to_summarize, to_keep) = {
            let mut mem = memory.write().unwrap();
            let system: Vec<_> = mem.messages.iter()
                .filter(|m| m.role == Role::System)
                .cloned()
                .collect();
            let non_system: Vec<_> = mem.messages.iter()
                .filter(|m| m.role != Role::System)
                .cloned()
                .collect();
            let split = non_system.len().saturating_sub(self.keep_last_n);
            let (old, recent) = non_system.split_at(split);
            (old.to_vec(), recent.to_vec())
        };

        if to_summarize.is_empty() {
            return;
        }

        // 2. Build summarisation prompt
        let conversation_text: String = to_summarize
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "Summarize this conversation segment in 2-3 paragraphs:\n\n{conversation_text}"
        );

        // 3. Call compact model synchronously (block_on inside hook)
        let runtime = tokio::runtime::Handle::current();
        let summary = runtime.block_on(async {
            let request = CompletionRequest {
                model: self.model.clone(),
                messages: vec![Message::new(Role::User, prompt)],
                tools: None,
                tool_choice: None,
                stream: false,
                temperature: Some(0.3),
                max_tokens: Some(1024),
            };
            let response = self.client.generate(request).await?;
            Ok::<_, ProviderError>(response.message.content)
        }).unwrap_or_else(|_| "[Summarisation failed]".into());

        // 4. Rebuild memory
        {
            let mut mem = memory.write().unwrap();
            let system: Vec<_> = mem.messages.iter()
                .filter(|m| m.role == Role::System)
                .cloned()
                .collect();
            mem.messages.clear();
            mem.messages.extend(system);
            mem.messages.push(Message::new(
                Role::System,
                format!("[Conversation summary]\n{summary}"),
            ));
            mem.messages.extend(to_keep);
        }
    }
}
```

> **Design note:** MacroCompaction uses `tokio::runtime::Handle::block_on`
> because `on_llm_start` is synchronous (the trait doesn't support async).
> This blocks the calling thread during summarisation, which is acceptable
> because it only fires occasionally (~every 2M characters).

---

## 7. Event System & TUI Architecture

### Complete `AgentEvent` Enum

```rust
// core/engine/src/events.rs
pub enum AgentEvent {
    /// Run is starting. user_input is the original user question.
    RunStarted { user_input: String },

    /// Streaming text token from the LLM. Arrives incrementally during
    /// SSE streaming — one event per token.
    Token(String),

    /// Streaming reasoning/chain-of-thought token (deepseek-r1, etc.).
    ReasoningToken(String),

    /// The agent is about to execute a tool call. origin distinguishes
    /// LLM-initiated calls from user-typed shell commands (! prefix).
    ToolCall {
        origin: CallOrigin,
        name: String,
        arguments: String,
    },

    /// Tool completed successfully.
    ToolSuccessful {
        origin: CallOrigin,
        name: String,
        output: String,
    },

    /// Tool was blocked by a hook.
    ToolRejected {
        origin: CallOrigin,
        name: String,
        reason: String,
    },

    /// Tool execution failed.
    ToolFailure {
        origin: CallOrigin,
        name: String,
        error: String,
    },

    /// Progress update from a long-running tool.
    ToolProgress {
        name: String,
        message: String,
    },

    /// User intervention needed. Consumer must send back InterveneResponse.
    NeedUserIntervene(InterveneRequest),

    /// Run completed normally.
    RunCompleted { answer: String },

    /// Run failed with an error.
    RunFailed { error: String },

    /// User cancelled the run.
    Cancelled,

    /// Terminal sentinel — always the last event. No more events follow.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOrigin {
    Llm,   // Tool call initiated by the LLM
    User,  // Tool call initiated by a user-typed ! command
}

#[derive(Debug)]
pub enum RunOutcome {
    Completed { answer: String },
    Failed { error: String },
    Cancelled,
}
```

### Single-Channel Topology

```
                    ┌──────────────────────┐
                    │       Agent          │
                    │                      │
                    │  ReAct Loop ─────────┼────→ mpsc::unbounded_channel ──→ TUI / Consumer
                    │                      │         ↑
                    │  Tool Execution ─────┼─────────┘
                    │                      │
                    │  Hooks (Sandbox) ────┼─────────┘
                    │                      │
                    │  User !commands ─────┼─────────┘
                    └──────────────────────┘
```

All events flow through a **single** `mpsc::unbounded_channel::<AgentEvent>`.
There is no separate "hook event channel" or "tool progress channel".  This
simplifies consumer code to a single match statement.

### TUI Consumption Pattern

```rust
use engine::AgentEvent;

async fn consume_events(mut rx: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
    let mut current_text = String::new();
    let mut active_tool_calls: HashMap<String, String> = HashMap::new();

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::Token(text) => {
                current_text.push_str(&text);
                render_streaming_text(&current_text);
            }
            AgentEvent::ReasoningToken(text) => {
                // Show in a separate "thinking" panel
                render_reasoning(&text);
            }
            AgentEvent::ToolCall { name, arguments, .. } => {
                active_tool_calls.insert(name.clone(), "Running...".into());
                render_tool_panel(&active_tool_calls);
            }
            AgentEvent::ToolSuccessful { name, output, .. } => {
                active_tool_calls.insert(name, format!("✅ {}", output));
                render_tool_panel(&active_tool_calls);
            }
            AgentEvent::ToolRejected { name, reason, .. } => {
                active_tool_calls.insert(name, format!("❌ Rejected: {}", reason));
                render_tool_panel(&active_tool_calls);
            }
            AgentEvent::ToolFailure { name, error, .. } => {
                active_tool_calls.insert(name, format!("❌ Failed: {}", error));
                render_tool_panel(&active_tool_calls);
            }
            AgentEvent::ToolProgress { name, message } => {
                active_tool_calls.insert(name, format!("⏳ {message}"));
                render_tool_panel(&active_tool_calls);
            }
            AgentEvent::NeedUserIntervene(request) => {
                let choice = show_user_prompt(&request.message, &request.options);
                request.responder.send(InterveneResponse {
                    chosen: choice,
                }).ok();
            }
            AgentEvent::RunCompleted { answer } => {
                render_final(answer);
            }
            AgentEvent::RunFailed { error } => {
                render_error(error);
            }
            AgentEvent::Cancelled => {
                render_message("Cancelled by user.");
            }
            AgentEvent::Done => break,
            _ => {}
        }
    }
}
```

---

## 8. WorkspaceFs Sandbox Internals

### Security Features

`WorkspaceFs` enforces a **chroot-like sandbox** for all file operations:

| Feature | Implementation | Configurable |
|---|---|---|
| Path canonicalization | `std::fs::canonicalize` before every access | No — always on |
| Workspace boundary check | All paths must be under `workspace_root` | No — always on |
| File-size caps | Reject reads/writes exceeding limits | `filesystem.max_read_bytes` / `max_write_bytes` |
| Extension blocklist | Reject writes to blocked extensions | `filesystem.blocked_write_extensions` |
| Hidden file protection | Reject writes to dot-files | `filesystem.forbid_hidden_file_writes` |
| Binary content detection | Reject files with null bytes | `filesystem.forbid_binary_writes` |
| TOCTOU re-checks | Re-canonicalize after file opens | No — always on |

### Path Resolution Flow

```
input: "src/../src/../../../etc/passwd"
  │
  ▼
Join with workspace_root: "/workspace/src/../src/../../../etc/passwd"
  │
  ▼
canonicalize(): "/etc/passwd"
  │
  ▼
Check: does "/etc/passwd" start with "/workspace"?
  │
  ▼
NO → Reject with FsError::PathNotInWorkspace
```

### Configuration File

```toml
# .loomis/config.toml
[filesystem]
max_read_bytes = 1_048_576     # 1 MiB
max_write_bytes = 524_288      # 512 KiB
forbid_binary_writes = true
forbid_hidden_file_writes = true
blocked_write_extensions = [".exe", ".dll", ".so", ".dylib", ".sys", ".bin"]

[shell]
default_timeout_secs = 30
max_timeout_secs = 120
max_output_bytes = 100_000
sanitize_environment = true

[shell.auto_approve]
prefixes = ["cargo", "git", "npm", "node", "python", "python3", "ls", "cat"]

[shell.deny_patterns]
patterns = ["rm\\s+-rf\\s+(/|~)", "sudo\\s+", "chmod\\s+[0-7]{3,4}\\s+/"]

[shell.allowed_commands]
binaries = []   # Empty = permissive mode

[quotas]
max_steps_per_session = 50
max_concurrent_shells = 2
max_total_operations = 10_000

[audit]
enabled = true
log_file = ".loomis/audit.jsonl"
```

Missing file → safe defaults.  All fields are optional.

---

## 9. Multi-Layer Sandbox Defense

The sandbox uses **defense in depth** — five independent layers:

```
                    User Request
                         │
                         ▼
Layer 1: WorkspaceFs ────┤ Path sandbox for file tools
Layer 2: ShellFilter ────┤ Command classification
Layer 3: SandboxHook ────┤ Permission checks + user prompts
Layer 4: ResourceTracker─┤ Quotas and limits
Layer 5: EnvSanitizer ───┤ Environment variable cleaning
                         │
                    Child Process
                         │
                    Watchdog ────┤ kills on timeout
```

### Layer 1: WorkspaceFs (Path Sandbox)

Covered in §8.  All file paths are canonicalized and bound-checked.

### Layer 2: ShellFilter (Command Classification)

```rust
// bins/loomis/src/sandbox/shell_filter.rs (conceptual)
enum Classification {
    AutoApprove,            // Safe prefix match
    Deny(String),           // Matches deny pattern
    NeedsApproval(String),  // Prompt user
}

fn classify(command: &str, config: &ShellConfig) -> Classification {
    // 1. Check deny patterns (regex) — reject immediately
    for pattern in &config.deny_patterns.patterns {
        if regex::Regex::new(pattern).unwrap().is_match(command) {
            return Classification::Deny(format!("Matches deny pattern: {pattern}"));
        }
    }

    // 2. If strict allowlist mode, check binary name
    if !config.allowed_commands.binaries.is_empty() {
        let binary = command.split_whitespace().next().unwrap_or("");
        if !config.allowed_commands.binaries.iter().any(|b| b == binary) {
            return Classification::Deny(format!("{binary} not in allowed list"));
        }
    }

    // 3. Check auto-approve prefixes
    let first_word = command.split_whitespace().next().unwrap_or("");
    for prefix in &config.auto_approve.prefixes {
        if first_word == prefix.as_str() {
            return Classification::AutoApprove;
        }
    }

    // 4. Everything else needs user approval
    Classification::NeedsApproval(command.into())
}
```

ShellFilter runs in **both** `before_tool_call` (Hook layer) **and**
`execute_stream` (Tool layer) as a dual defense.

### Layer 3: SandboxHook (Orchestrator)

`SandboxHook` is the central security coordinator:

```rust
impl AgentHook for SandboxHook {
    fn before_tool_call(
        &self,
        session_id: &str,
        name: &str,
        arguments: &str,
        origin: CallOrigin,
    ) -> Result<(), String> {
        // 1. Check ResourceTracker quotas
        self.tracker.check_quota()?;

        // 2. If it's a shell command, classify via ShellFilter
        if name == "shell" {
            match self.shell_filter.classify(arguments) {
                Classification::AutoApprove => {}, // Pass through
                Classification::Deny(reason) => {
                    self.audit.log_denied(session_id, name, &reason);
                    return Err(reason);
                }
                Classification::NeedsApproval(cmd) => {
                    // Prompt user via InterveneRequest
                    let approved = self.prompt_user(&cmd)?;
                    if !approved {
                        self.audit.log_denied(session_id, name, "User denied");
                        return Err("User denied".into());
                    }
                }
            }
        }

        // 3. If it's a write, check extension blocklist
        if name == "write" {
            let args: WriteArgs = serde_json::from_str(arguments)
                .map_err(|e| format!("Invalid args: {e}"))?;
            // ... extension checks ...
        }

        Ok(())
    }

    fn after_tool_call(
        &self,
        session_id: &str,
        name: &str,
        output: &str,
        origin: CallOrigin,
    ) {
        // Update ResourceTracker counters
        self.tracker.record(name, output.len());

        // Log to audit trail
        self.audit.log_success(session_id, name, output);
    }
}
```

### Layer 4: ResourceTracker (Quotas)

```rust
struct ResourceTracker {
    total_operations: AtomicUsize,
    active_shells: AtomicUsize,
    max_total: usize,
    max_shells: usize,
    start_time: Instant,
}

impl ResourceTracker {
    fn check_quota(&self) -> Result<(), String> {
        if self.total_operations.load(Ordering::Relaxed) >= self.max_total {
            return Err("Total operation quota exceeded".into());
        }
        if self.active_shells.load(Ordering::Relaxed) >= self.max_shells {
            return Err("Max concurrent shells reached".into());
        }
        Ok(())
    }
}
```

### Layer 5: EnvSanitizer (Process Safety)

Before spawning any child process, the environment is cleaned:

```rust
fn sanitize_env() -> HashMap<String, String> {
    let safe_keys = [
        "PATH", "HOME", "USER", "TEMP", "TMP", "TMPDIR",
        "LANG", "LC_ALL", "TERM", "COLORTERM",
        "CARGO_HOME", "RUSTUP_HOME",
        // ... minimal set for builds to work
    ];

    let mut env = HashMap::new();
    for key in safe_keys {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    env
}
```

The original environment is completely cleared, and only a safe allowlist
is restored.

### Watchdog (Timeout)

```rust
// Spawn process, then race against a timeout
let process = Command::new("sh").arg("-c").arg(&cmd)
    .env_clear()
    .envs(&sanitized_env)
    .spawn()?;

let result = tokio::time::timeout(
    Duration::from_secs(timeout_secs),
    process.wait_with_output(),
).await;

match result {
    Ok(Ok(output)) => Ok(output),
    Ok(Err(e)) => Err(e),
    Err(_elapsed) => {
        // Kill the process tree
        kill_process_tree(process.id());
        Err("Command timed out")
    }
}
```

On Windows: `taskkill /F /T /PID {pid}`.  On Unix: `kill(-pid, SIGKILL)`.

---

## 10. Subagent System

### Architecture

```
Parent Agent
  │
  ├─ Memory (conversation history)
  │
  ├─ ToolRegistry ────────┐
  │   read, grep, glob,   │
  │   calculator, write,  │
  │   shell, task ←───────┤ (SubagentTool wraps a child Agent)
  │                       │
  └─ [LLM calls "task"] ──┤
                          │
                          ▼
                    SubagentTool<C>.execute_stream()
                          │
                          │  Creates NEW Memory (isolated)
                          │  Filters tools (read-only subset)
                          │  Configures via SubagentConfig
                          │
                          ▼
                    Child Agent
                      │
                      ├─ Memory: [system prompt, inherited context*, user prompt]
                      ├─ Tools:  read, grep, glob, ls, calculator (no write/shell/task)
                      │
                      ├─ [ReAct Loop — up to max_steps iterations]
                      │
                      └─ Result → ProgressStream → back to parent
```

*Inherited context: if `SubagentConfig::inherit_context_messages` is set,
the last N non-System messages from the parent's memory are copied into
the child's initial memory.

### `SubagentTool` Implementation

```rust
#[tool(
    name = "task",
    description = "Delegate a complex task to a sub-agent with read-only ...",
    args = TaskArgs
)]
pub struct SubagentTool<C: LLMClient + Clone + 'static> {
    llm: C,
    config: SubagentConfig,
    subagent_tools: Arc<ToolRegistry>,
    parent_memory: SharedMemory,
}

impl<C: LLMClient + Clone + 'static> SubagentTool<C> {
    pub fn new(
        llm: C,
        config: SubagentConfig,
        subagent_tools: Arc<ToolRegistry>,
        parent_memory: SharedMemory,
    ) -> Self { ... }

    // execute_stream spawns a child Agent:
    // 1. Clone the LLM client
    // 2. Build an EngineContext with isolated Memory
    // 3. Run the child agent
    // 4. Stream results as Progress events
}
```

### `SubagentConfig`

```rust
pub struct SubagentConfig {
    pub model: String,                           // Required
    pub system_prompt: String,                   // Default: read-only assistant
    pub max_steps: usize,                        // Default: 25
    pub max_retries: usize,                      // Default: 2
    pub streaming: bool,                         // Default: true
    pub timeout_secs: Option<u64>,              // Default: Some(120)
    pub inherit_context_messages: Option<usize>, // Default: None
}
```

### `filter_tools()` — Creating a Read-Only Registry

```rust
use subagent::filter_tools;
use std::sync::Arc;

// Parent has: read, write, edit, grep, glob, ls, calculator, shell, task
let parent_registry = parent_agent.tools();

// Subagent gets: read, grep, glob, ls, calculator (no write, shell, or task!)
let subagent_tools = Arc::new(filter_tools(
    &parent_registry,
    &["read", "grep", "glob", "ls", "calculator"],
));

// Critical: never include "task" — prevents infinite subagent recursion
```

### Usage Example

```rust
use subagent::{SubagentTool, SubagentConfig, filter_tools};

let subagent_tools = Arc::new(filter_tools(tools, &["read", "grep", "glob", "calculator"]));

let subagent = SubagentTool::new(
    client.clone(),
    SubagentConfig {
        model: "deepseek-v4-flash".into(),
        timeout_secs: Some(60),
        ..Default::default()
    },
    subagent_tools,
    memory.clone(),
);

let agent = Agent::builder(client, "deepseek-chat")
    .tool(subagent)
    // ... other tools ...
    .build();
```

### Worker-Thread Pattern

`SubagentTool::execute_stream()` spawns a dedicated OS thread that runs the
child agent to completion.  Progress updates flow through a `tokio::sync::mpsc`
channel back to the main runtime.  A timeout guard (`tokio::select!` or
`tokio::time::timeout`) kills the thread if it runs too long.

---

## 11. Conversation Persistence

### API

```rust
use memory::{save_conversation, load_conversation, list_threads};
use std::path::Path;

// Save current conversation
let workspace = Path::new("/my/project");
save_conversation(&memory, workspace, "debug-session").await?;

// List saved threads
let threads = list_threads(workspace)?;
for t in threads {
    println!("{} — {} messages", t.name, t.message_count);
}

// Load a thread
let memory = load_conversation(workspace, "debug-session")?;
```

### File Formats

Two files per thread in `.loomis/threads/`:

**`{name}.json`:** Machine-readable, complete message history with all fields.

```json
[
  {
    "role": "system",
    "content": "You are a helpful assistant...",
    "tool_calls": null,
    "tool_call_id": null,
    "name": null
  },
  {
    "role": "user",
    "content": "What is Rust?",
    ...
  }
]
```

**`{name}.md`:** Human-readable markdown for browsing.

```markdown
# Thread: debug-session
Saved: 2026-07-13T19:43:49Z
Messages: 42

---

## System
You are a helpful assistant...

## User
What is Rust?

## Assistant
Rust is a systems programming language...
```

### Auto-Save

In `bins/loomis/`, conversation is auto-saved after each agent turn.  The
`sandbox_hook` triggers the save in `on_run_finish`.

---

## 12. Complete Reference Example — Code Reviewer

A fully assembled agent that reviews code.  This example uses the advanced
`EngineContext` API, manual hook registration, and subagents.

```rust
use deepseek::DeepSeekClient;
use engine::{Agent, AgentEvent, EngineContext};
use hooks::{MicroCompactHook, MacroCompactHook};
use memory::{Memory, SharedMemory};
use subagent::{SubagentTool, SubagentConfig, filter_tools};
use std::sync::Arc;
use tools::ToolRegistry;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── LLM Clients ────────────────────────────────────────────────────
    let main_client = DeepSeekClient::from_env()?;

    // ── Memory ─────────────────────────────────────────────────────────
    let mut memory = Memory::new();
    memory.push(provider::Message::new(
        provider::Role::System,
        "You are an expert code reviewer. Analyze code for bugs, security \
         issues, and style problems. Use the read tool to inspect files, \
         grep to search for patterns, and task for complex investigations.",
    ));
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(memory));

    // ── Tool Registry ──────────────────────────────────────────────────
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadTool::new(workspace_root.clone())));
    registry.register(Arc::new(GrepTool::new(workspace_root.clone())));
    registry.register(Arc::new(GlobTool::new(workspace_root.clone())));
    registry.register(Arc::new(LsTool::new(workspace_root.clone())));
    registry.register(Arc::new(CalculatorTool));
    let registry = Arc::new(registry);

    // ── Subagent ───────────────────────────────────────────────────────
    let subagent_tools = Arc::new(filter_tools(
        &registry,
        &["read", "grep", "glob", "ls", "calculator"],
    ));

    let subagent = SubagentTool::new(
        main_client.clone(),
        SubagentConfig {
            model: "deepseek-v4-flash".into(),
            timeout_secs: Some(120),
            inherit_context_messages: Some(3),
            max_steps: 25,
            system_prompt: "Expert code reviewer sub-agent...".into(),
            ..Default::default()
        },
        subagent_tools,
        memory.clone(),
    );

    // Add subagent to main registry
    let registry_mut = Arc::get_mut(&mut registry_clone)?;
    registry_mut.register(Arc::new(subagent));
    // Actually, since registry is Arc, clone and add:
    let mut full_registry = ToolRegistry::new();
    for (name, tool) in registry.iter() {
        full_registry.register(Arc::clone(tool));
    }
    full_registry.register(Arc::new(subagent));
    let registry = Arc::new(full_registry);

    // ── Compaction Hooks ───────────────────────────────────────────────
    let micro = MicroCompactHook::default();
    let macro_compact = MacroCompactHook::new(
        main_client.clone(),
        "deepseek-v4-flash",
    );

    // ── Engine Context ─────────────────────────────────────────────────
    let ctx = EngineContext::builder(
        main_client.clone(),
        memory.clone(),
        registry,
        "deepseek-chat",
    )
    .hook(micro)
    .hook(macro_compact)
    .max_steps(100)
    .max_retries(5)
    .build();

    // ── Agent ──────────────────────────────────────────────────────────
    let agent = Agent::new(ctx);

    // ── Event Channel ──────────────────────────────────────────────────
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    let handle = tokio::spawn(async move {
        agent.run_with_events(
            "Review all .rs files in src/ for potential issues. \
             Check for: error handling gaps, unsafe code, resource leaks, \
             and inconsistent style. Use subagents for deep investigation \
             of complex files.",
            tx,
        ).await
    });

    // ── Consumer ───────────────────────────────────────────────────────
    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::Token(text) => print!("{text}"),
            AgentEvent::ToolCall { name, args, .. } => {
                let preview: String = args.chars().take(80).collect();
                eprintln!("\n[TOOL] {name}({preview}...)");
            }
            AgentEvent::ToolProgress { name, message } => {
                let preview: String = message.chars().take(60).collect();
                eprintln!("  ⏳ {name}: {preview}");
            }
            AgentEvent::ToolSuccessful { name, .. } => {
                eprintln!("  ✅ {name} done");
            }
            AgentEvent::ToolFailure { name, error, .. } => {
                eprintln!("  ❌ {name}: {error}");
            }
            AgentEvent::RunCompleted { answer } => {
                println!("\n\n=== Review Complete ===\n{answer}");
            }
            AgentEvent::RunFailed { error } => {
                eprintln!("\n=== Review Failed ===\n{error}");
            }
            AgentEvent::Done => break,
            _ => {}
        }
    }

    let _ = handle.await?;
    Ok(())
}
```

---

## 13. Appendices

### A. Design Principles

1. **Composability.** Each component (tool, hook, provider) is an independent
   crate with well-defined boundaries.  You can replace any piece without
   touching the rest.
2. **Safety by default.** WorkspaceFs, ShellFilter, EnvSanitizer are always
   active.  Configuration tightens constraints — it never loosens them.
3. **Progressive complexity.** `Agent::builder()` for beginners,
   `EngineContext::builder()` for advanced users, raw `Agent::new(EngineContext)`
   for total control.
4. **Single-channel events.** One `AgentEvent` type, one `mpsc` channel.  No
   separate channels for tools, LLM streaming, hooks, or user commands.
5. **Sync traits, async execution.** `Tool` and `AgentHook` are sync and
   object-safe.  Async work is delegated to spawned threads that bridge through
   channels.

### B. Implementation Checklists

#### When implementing a new LLM Provider:
- [ ] Implement `LLMClient::generate` — convert `CompletionRequest` → API call → `CompletionResponse`
- [ ] Implement `LLMClient::stream` — SSE parsing, convert each event → `StreamChunk`
- [ ] Handle `ProviderError` variants correctly (Http, RateLimited, Authentication, etc.)
- [ ] Map tool calls: provider's response format → `Vec<ToolCall>`
- [ ] Map tool definitions: `Vec<ToolDef>` → provider's tool/function format
- [ ] Test with tool calls (LLM must successfully call and receive results)

#### When implementing a new Tool:
- [ ] Define args struct with `#[derive(JsonSchema, Deserialize)]` + `#[serde(deny_unknown_fields)]`
- [ ] Add `#[tool(name = ..., description = ..., args = ...)]` on struct
- [ ] Implement `execute_stream(&self, args: Args) -> Result<ProgressStream, ToolError>`
- [ ] For simple tools: return `ProgressStream::done(result)`
- [ ] For long-running tools: spawn thread, use `mpsc::channel`, return `ProgressStream::from_receiver(rx)`
- [ ] Map errors to correct `ToolError` variant (InvalidArgs, Execution, Fs, PermissionDenied)
- [ ] Register with `agent.tool(MyTool)` or `registry.register(Arc::new(MyTool))`
- [ ] Test: tool appears in LLM request, LLM successfully calls it

#### When implementing a new Hook:
- [ ] Choose which lifecycle callbacks to override (empty no-ops by default)
- [ ] For security hooks: implement `before_tool_call` → block/reject
- [ ] For compaction hooks: implement `on_llm_start` → mutate memory
- [ ] For logging/metrics: implement `on_step_start`, `on_llm_end`, `after_tool_call`
- [ ] For user approval: use `InterveneRequest`/`InterveneResponse` in `before_tool_call`
- [ ] Register with `agent.hook(MyHook)` or `EngineContext::builder().hook(MyHook)`

### C. Code Reference Map

| Want to... | Read this file |
|---|---|
| Understand the ReAct loop | `core/engine/src/agent.rs` |
| See how tools are defined | `bins/loomis/src/tools/calculator.rs` |
| See how sandbox works | `bins/loomis/src/sandbox/` |
| See how TUI renders events | `bins/loomis/src/tui/` |
| See how agent is assembled | `bins/loomis/src/agent_setup.rs` |
| Understand SSE streaming | `core/deepseek/src/client.rs` |
| Understand compaction | `extensions/compact/src/compact.rs` |
| Understand subagents | `extensions/subagent/src/tool.rs` |
| Understand the Tool trait | `core/tools/src/tool.rs` |
| Understand the hook lifecycle | `core/engine/src/hooks.rs` |
| Understand events | `core/engine/src/events.rs` |
| Understand sandbox config | `extensions/sandbox/src/config.rs` |

### D. Glossary

| Term | Definition |
|---|---|
| **ReAct loop** | Reasoning + Acting loop: LLM → tool calls → results → LLM → ... |
| **Tool** | A Rust function exposed to the LLM. Implemented via `Tool` trait. |
| **Hook** | Lifecycle callback implementing `AgentHook`. For logging, sandbox, compaction. |
| **Compaction** | Reducing conversation size. Micro = prune tool outputs. Macro = LLM summarise. |
| **WorkspaceFs** | Sandboxed filesystem — all paths bound to workspace root. |
| **ShellFilter** | Command classifier — auto-approve, deny, or prompt. |
| **ProgressStream** | Returns from tool execution. Visited to get progress / final result. |
| **SharedMemory** | `Arc<RwLock<Memory>>` — shared, thread-safe conversation buffer. |
| **EngineContext** | Container for all agent dependencies (LLM, Memory, Tools, Hooks). |
| **Subagent** | A child agent spawned as a tool. Has isolated memory and filtered tools. |
| **InterveneRequest** | Rendez-vous prompt sent through event channel for user approval. |
| **CallOrigin** | Whether a tool call came from `LLM` or `User` (`!` command). |
