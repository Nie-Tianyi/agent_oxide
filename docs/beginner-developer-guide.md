# Loomis Beginner Developer Guide

> **Build your first AI agent in 10 minutes.**  
> No prior agent experience needed — just basic Rust (closures, async, `Arc`).

---

## Welcome

Loomis is a **Rust agent framework** that turns an LLM into a fully autonomous
tool-using assistant.  Give it tools — a calculator, file reader, shell access,
web search — and the LLM will use them to solve real problems, step by step.

This guide takes you from zero to a working agent.  Each chapter builds on the
last: you'll start with a "hello world" conversation, add tools one by one, and
finish with a complete project.  No magic, no hidden complexity — everything is
explained as you encounter it.

**What you'll build:**

- A conversational agent with custom tools
- A file reader that respects workspace boundaries
- Real-time streaming output
- A complete Todo CLI agent by the end

Let's go.

---

## Chapter 1: Getting Ready

### What you need

| Prerequisite | How to check |
|---|---|
| **Rust** (latest stable) | `rustc --version` |
| **DeepSeek API key** | Create one at [platform.deepseek.com](https://platform.deepseek.com) |
| **This workspace** | You're already in it! |

Set your API key:

```bash
echo "DEEPSEEK_API=sk-your-key-here" > .env
```

`dotenvy` loads `.env` automatically — your API key never appears in source code.

### Create your example binary

We'll add a tutorial binary to the workspace.  Create the directory structure:

```bash
mkdir -p bins/tutorial/src
```

**`bins/tutorial/Cargo.toml`:**

```toml
[package]
name = "tutorial"
version = "0.1.0"
edition = "2024"

[dependencies]
engine = { path = "../../core/engine" }
deepseek = { path = "../../core/deepseek" }
tools = { path = "../../core/tools" }
tools-macros = { path = "../../core/tools-macros" }
memory = { path = "../../core/memory" }
hooks = { path = "../../extensions/compact" }
provider = { path = "../../core/provider" }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "1"
```

**`bins/tutorial/src/main.rs`:**
```rust
fn main() {
    println!("Tutorial binary ready!");
}
```

Verify it builds:

```bash
cargo build -p tutorial
```

> **Done?** You have a clean canvas.  Let's write our first agent.

---

## Chapter 2: Hello, Agent! — Your First Conversation

### The Simplest Possible Agent

Replace `bins/tutorial/src/main.rs` with:

```rust
use deepseek::DeepSeekClient;
use engine::Agent;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create the LLM provider — it actually talks to the API.
    let client = DeepSeekClient::from_env()?;

    // 2. Build the agent: client + model name.
    let agent = Agent::builder(client, "deepseek-chat")
        .system_prompt("You are a helpful assistant. Keep answers short.")
        .build();

    // 3. Run it with a question.  This is async — .await it.
    let answer = agent.run("What is Rust's ownership model in one sentence?").await?;

    println!("Answer: {}", answer);
    Ok(())
}
```

Run it:

```bash
cargo run -p tutorial
```

You'll see something like:

```
Answer: Rust's ownership model ensures memory safety at compile time by enforcing
that each value has exactly one owner at any given time...
```

### What just happened?

Let me walk you through those 3 steps:

```
Step 1: DeepSeekClient::from_env()
        ┌──────────────────────────────────┐
        │  Reads DEEPSEEK_API from .env    │
        │  Creates an HTTP client          │
        │  Implements LLMClient trait      │
        └──────────────────────────────────┘

Step 2: Agent::builder(client, "deepseek-chat")
        .system_prompt("...")
        .build()
        ┌──────────────────────────────────┐
        │  Creates an empty Memory buffer  │
        │  Pushes system prompt into it    │
        │  Creates a ToolRegistry (empty)  │
        │  Wraps everything in Agent       │
        └──────────────────────────────────┘

Step 3: agent.run("What is Rust's ownership model...?")
        ┌──────────────────────────────────┐
        │  Pushes user message to memory   │
        │  Sends full context to LLM        │
        │  Receives response (streaming)   │
        │  No tool calls → final answer    │
        │  Returns the text                │
        └──────────────────────────────────┘
```

The agent manages conversation history automatically.  Every `run()` call
pushes the user's question, sends the full history to the LLM, and stores
the response.  Call `run()` again and the LLM remembers everything.

> **Key insight:** `Agent::builder(client, model)` is the "batteries included"
> entry point.  It handles memory, tool registry, hooks — everything you need
> to get started.

---

## Chapter 3: Giving Your Agent Tools

An agent without tools is just a chatbot.  **Tools are what make agents powerful.**

A tool is a Rust struct that implements the `Tool` trait.  The `#[tool]` macro
generates that implementation for you — you just define the struct, its
arguments, and the `execute_stream` method.

### The `#[tool]` Macro in 30 Seconds

```rust
use schemars::JsonSchema;
use serde::Deserialize;
use tools::{tool, ProgressStream, ToolError};

// 1. Define your argument struct
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct MyArgs {
    pub x: i32,
    pub y: i32,
}

// 2. Annotate your tool struct
#[tool(
    name = "my_tool",
    description = "Does something useful with x and y.",
    args = MyArgs
)]
struct MyTool;

// 3. Implement the logic
impl MyTool {
    fn execute_stream(&self, args: MyArgs) -> Result<ProgressStream, ToolError> {
        let result = format!("x + y = {}", args.x + args.y);
        Ok(ProgressStream::done(result))
    }
}
```

That's it!  Three parts:
1. **Args struct** — what the LLM passes to your tool (uses Serde to deserialize JSON)
2. **`#[tool(...)]`** — declares the tool's name, description, and argument type
3. **`execute_stream()`** — your logic, returns `ProgressStream::done(output)` for simple tools

### Our First Tool: Calculator

Let's build a real calculator that handles arbitrary expressions.  Update
`bins/tutorial/src/main.rs`:

```rust
use deepseek::DeepSeekClient;
use engine::Agent;
use schemars::JsonSchema;
use serde::Deserialize;
use tools::{tool, ProgressStream, ToolError};

// ── Calculator Tool ──────────────────────────────────────────────────────────

#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalculatorArgs {
    /// A mathematical expression like "2 + 3 * 4" or "(100 - 20) / 4".
    #[schemars(description = "A mathematical expression using +, -, *, /, parentheses.")]
    pub expression: String,
}

#[tool(
    name = "calculator",
    description = "Evaluate a mathematical expression. Supports +, -, *, /, and \
                   parentheses. Example: '2 + 3 * 4' returns 14.",
    args = CalculatorArgs
)]
struct CalculatorTool;

impl CalculatorTool {
    fn execute_stream(&self, args: CalculatorArgs) -> Result<ProgressStream, ToolError> {
        // This is a simplified evaluator.  In production you'd use a proper
        // expression parser, but this demonstrates the pattern.
        let result = simple_eval(&args.expression)?;
        Ok(ProgressStream::done(result))
    }
}

fn simple_eval(expr: &str) -> Result<String, ToolError> {
    // For demo purposes — in real code, use a proper evaluator.
    // We'll keep it simple: just return the expression as-is.
    // The LLM will understand this limitation.
    Ok(format!("Expression received: {}", expr))
}

// ── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = DeepSeekClient::from_env()?;

    let agent = Agent::builder(client, "deepseek-chat")
        .system_prompt("You are a math assistant. Use the calculator tool for any math problem.")
        .tool(CalculatorTool)   // <-- Register the tool!
        .build();

    let answer = agent.run("What is 15 * 7 + 3?").await?;
    println!("Answer: {}", answer);
    Ok(())
}
```

Run it and watch the agent use your tool:

```bash
cargo run -p tutorial
```

Under the hood, this is what happens:

```
User: "What is 15 * 7 + 3?"
  │
  ▼
Agent sends to LLM (with tool definitions)
  │
  ▼
LLM responds: "I'll use the calculator tool."
  │  tool_calls: [{ name: "calculator", arguments: {"expression": "15 * 7 + 3"} }]
  │
  ▼
Agent executes CalculatorTool::execute_stream()
  │  returns: "Expression received: 15 * 7 + 3"
  │
  ▼
Agent sends tool result back to LLM
  │
  ▼
LLM responds with final answer (using the tool result)
  │
  ▼
Agent returns: "15 * 7 + 3 = 108"
```

This cycle — **call LLM → execute tools → send results → repeat** — is called the
**ReAct loop** (Reasoning + Acting).  It continues until the LLM produces a text
answer without requesting more tool calls.

---

## Chapter 4: How Everything Fits Together

Now that you've built something, let's step back and understand the architecture.
You don't need to memorize this — bookmark it for reference.

### The Crate Map (simplified)

```
┌─────────────────────────────────────────────────────┐
│                  Your Binary (tutorial)              │
│  ┌─────────┐  ┌──────────┐  ┌────────────────────┐ │
│  │ Tools   │  │  Hooks   │  │  Agent::builder()  │ │
│  │ (your   │  │ (your    │  │  .tool() .hook()   │ │
│  │  logic) │  │  policy) │  │  .system_prompt()  │ │
│  └────┬────┘  └────┬─────┘  └─────────┬──────────┘ │
└───────┼────────────┼──────────────────┼─────────────┘
        │            │                  │
        ▼            ▼                  ▼
┌───────────────┐ ┌──────────────┐ ┌─────────────────┐
│ tools         │ │ hooks        │ │ engine          │
│ Tool trait    │ │ MicroCompact │ │ Agent           │
│ ToolRegistry  │ │ MacroCompact │ │ AgentHook trait │
│ ProgressStream│ │              │ │ AgentEvent      │
│ WorkspaceFs   │ │              │ │ AgentBuilder    │
└───────┬───────┘ └──────────────┘ └────────┬────────┘
        │                                    │
        │                                    ▼
        │                           ┌─────────────────┐
        │                           │ memory          │
        │                           │ Memory buffer   │
        │                           │ SharedMemory    │
        │                           └────────┬────────┘
        │                                    │
        ▼                                    ▼
┌─────────────────────────────────────────────────────┐
│ provider                                            │
│ LLMClient trait (generate / stream)                 │
└───────────────────────┬─────────────────────────────┘
                        │
                        ▼
                ┌───────────────┐
                │ deepseek      │
                │ DeepSeekClient│
                │ (implements   │
                │  LLMClient)   │
                └───────────────┘
```

**What matters for you:**
- **`tools`** — the `#[tool]` macro and `Tool` trait live here
- **`engine`** — `Agent`, `Agent::builder()`, `AgentHook`, `AgentEvent`
- **`memory`** — conversation history (`SharedMemory`)
- **`provider` / `deepseek`** — the LLM API abstraction

### The ReAct Loop

Every agent conversation follows this pattern:

```
     User Input
         │
         ▼
  ┌──────────────┐
  │  Push to     │   Memory: [system, user]
  │  Memory      │
  └──────┬───────┘
         │
         ▼
  ┌──────────────┐
  │  Send to     │   Full conversation history + tool definitions
  │  LLM         │
  └──────┬───────┘
         │
         ▼
  ┌────────────────────────────────────┐
  │  LLM Response                     │
  │  ┌─────────────────────────────┐  │
  │  │ Has tool calls?             │  │
  │  │  YES → execute tools        │──┼──→ tool results → push to memory → ⤴ LOOP
  │  │                               │  │
  │  │  NO  → final text answer     │──┼──→ DONE
  │  └─────────────────────────────┘  │
  └────────────────────────────────────┘
```

The LLM decides whether to use a tool or give a final answer.  Your job is to
provide useful tools and a clear system prompt.

---

## Chapter 5: Letting Your Agent Read Files

Real agents need to interact with files.  Loomis provides `WorkspaceFs` — a
sandboxed file system that keeps the agent inside the workspace root.

### WorkspaceFs Basics

```rust
use tools::WorkspaceFs;

let fs = WorkspaceFs::new("/path/to/workspace").unwrap();

// Read a file — path is relative to workspace root
let content = fs.read_to_string("src/main.rs").unwrap();

// Write a file
fs.write("output.txt", "hello world").unwrap();

// List directory
for entry in fs.read_dir(".").unwrap() {
    println!("{}", entry.path().display());
}
```

All paths are **canonicalized** and **checked** against the workspace root.
The agent cannot escape the sandbox.

### Building a ReadTool

```rust
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tools::{tool, ProgressStream, ToolError, WorkspaceFs};

// ── ReadTool ─────────────────────────────────────────────────────────────────

#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    /// Path to the file to read, relative to the workspace root.
    #[schemars(description = "Path to the file, relative to workspace root.")]
    pub file_path: String,
}

#[tool(
    name = "read",
    description = "Read the contents of a file in the workspace.",
    args = ReadArgs
)]
struct ReadTool {
    fs: WorkspaceFs,
}

impl ReadTool {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            fs: WorkspaceFs::new(workspace_root).expect("valid workspace root"),
        }
    }

    fn execute_stream(&self, args: ReadArgs) -> Result<ProgressStream, ToolError> {
        let content = self
            .fs
            .read_to_string(&args.file_path)
            .map_err(|e| ToolError::Fs(e.to_string()))?;
        Ok(ProgressStream::done(content))
    }
}
```

### Putting It Together

```rust
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = DeepSeekClient::from_env()?;

    // The workspace root is where the agent can read/write files.
    let workspace = env::current_dir()?;

    let agent = Agent::builder(client, "deepseek-chat")
        .system_prompt("You are a coding assistant. Use the read tool to inspect files.")
        .tool(CalculatorTool)
        .tool(ReadTool::new(workspace))  // <-- Agent can now read files!
        .build();

    let answer = agent
        .run("Read Cargo.toml and tell me what dependencies this project has.")
        .await?;
    println!("Answer: {}", answer);
    Ok(())
}
```

> **Security note:** `WorkspaceFs` enforces that the agent can only access
> files inside the workspace root.  Path traversal (`../../../etc/passwd`) is
> automatically blocked.  This is safe by default — no configuration needed.

---

## Chapter 6: Watching the Magic — Hooks & Events

You've built an agent that can think and act.  Now let's **watch** it.

### Hooks: Observing Every Step

A hook is a type that implements `AgentHook`.  Every method has a default
no-op — you only override what you care about.

```rust
use engine::AgentHook;
use memory::SharedMemory;
use provider::Message;

struct LoggingHook;

impl AgentHook for LoggingHook {
    fn on_step_start(&self, _session_id: &str, step: usize, max: usize) {
        eprintln!("[Step {step}/{max}]");
    }

    fn on_llm_end(&self, _session_id: &str, response: &Message) {
        let has_tools = response.tool_calls
            .as_ref()
            .map(|tcs| !tcs.is_empty())
            .unwrap_or(false);
        if has_tools {
            eprintln!("  → LLM requested tool calls");
        } else {
            let preview: String = response.content.chars().take(80).collect();
            eprintln!("  → LLM: {preview}...");
        }
    }
}
```

Register it:

```rust
let agent = Agent::builder(client, "deepseek-chat")
    .system_prompt("...")
    .tool(CalculatorTool)
    .hook(LoggingHook)  // <-- Watch everything!
    .build();
```

### Events: Streaming Real-Time Output

For TUIs and interactive apps, you want to see text **as it's generated**,
not after the whole response is ready.  Use `run_with_events()`:

```rust
use engine::AgentEvent;
use tokio::sync::mpsc;

let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

// Run the agent in a separate task
let agent_clone = &agent;
let handle = tokio::spawn(async move {
    agent_clone.run_with_events("What is 2+2?", tx).await
});

// Consume events in real time
while let Some(event) = rx.recv().await {
    match event {
        AgentEvent::Token(text) => {
            // Text arrives character-by-character (or token-by-token)
            print!("{text}");
        }
        AgentEvent::ToolCall { name, arguments, .. } => {
            println!("\n🔧 Calling tool: {name}({arguments})");
        }
        AgentEvent::ToolSuccessful { name, output, .. } => {
            println!("✅ {name}: {output}");
        }
        AgentEvent::RunCompleted { answer } => {
            println!("\n\n✅ Done: {answer}");
        }
        AgentEvent::Done => break,
        _ => {}
    }
}

let result = handle.await??;
```

Run this and you'll see the LLM's text appear in real time, tool calls
announced as they happen, and the final result.

> **Key insight:** There's only **one event channel**.  Both LLM tokens and
> tool execution events flow through the same `mpsc::unbounded_channel`.
> This simplifies TUI rendering — one consumer, one message type.

### Quick Reference: Event Types

| Event | Meaning |
|---|---|
| `Token(text)` | Text chunk from LLM (streaming) |
| `ReasoningToken(text)` | Chain-of-thought from LLM |
| `ToolCall { name, arguments, .. }` | About to execute a tool |
| `ToolSuccessful { output, .. }` | Tool completed successfully |
| `ToolRejected { reason, .. }` | Tool blocked by a hook |
| `ToolFailure { error, .. }` | Tool execution failed |
| `ToolProgress { message, .. }` | Real-time progress from long-running tool |
| `RunStarted { user_input }` | New conversation started |
| `RunCompleted { answer }` | Agent finished successfully |
| `RunFailed { error }` | Agent hit an error |
| `Cancelled` | User interrupted the agent |
| `Done` | Terminal event — always the last one |

---

## Chapter 7: Complete Project — Todo CLI Agent

Let's put everything together.  We'll build an agent that manages a todo list
in a text file, using the calculator, file reader, and a custom todo tool.

### What We're Building

```
User: "Add 'Buy groceries' to my todo list, then add 'Call dentist', and show me
       the list sorted.  Also, what's 5 * the number of items?"

Agent:
  1. Calls todo tool to add "Buy groceries"
  2. Calls todo tool to add "Call dentist"
  3. Calls read tool to read the todo file
  4. Calls calculator to compute 5 * 2
  5. Returns the answer
```

### Step 1: The TodoTool

```rust
use schemars::JsonSchema;
use serde::Deserialize;
use std::fs;
use tools::{tool, ProgressStream, ToolError};

#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoArgs {
    /// Action: "add", "list", or "remove"
    #[schemars(description = "One of: add, list, remove")]
    pub action: String,
    /// The todo item text (required for "add" and "remove")
    #[schemars(description = "The todo item text")]
    pub item: Option<String>,
}

#[tool(
    name = "todo",
    description = "Manage a todo list stored in todo.txt. Actions: add <item>, \
                   list (show all items), remove <item>.",
    args = TodoArgs
)]
struct TodoTool {
    todo_path: String,
}

impl TodoTool {
    fn new(todo_path: String) -> Self {
        // Ensure the file exists
        if !std::path::Path::new(&todo_path).exists() {
            fs::write(&todo_path, "").ok();
        }
        Self { todo_path }
    }

    fn execute_stream(&self, args: TodoArgs) -> Result<ProgressStream, ToolError> {
        match args.action.as_str() {
            "add" => {
                let item = args.item.ok_or_else(|| {
                    ToolError::InvalidArgs("'item' is required for 'add'".into())
                })?;
                let mut content = fs::read_to_string(&self.todo_path)
                    .map_err(|e| ToolError::Fs(e.to_string()))?;
                content.push_str(&format!("- [ ] {}\n", item));
                fs::write(&self.todo_path, &content)
                    .map_err(|e| ToolError::Fs(e.to_string()))?;
                Ok(ProgressStream::done(format!("Added: {}", item)))
            }
            "list" => {
                let content = fs::read_to_string(&self.todo_path)
                    .map_err(|e| ToolError::Fs(e.to_string()))?;
                if content.trim().is_empty() {
                    Ok(ProgressStream::done("Todo list is empty.".into()))
                } else {
                    Ok(ProgressStream::done(content))
                }
            }
            "remove" => {
                let item = args.item.ok_or_else(|| {
                    ToolError::InvalidArgs("'item' is required for 'remove'".into())
                })?;
                let content = fs::read_to_string(&self.todo_path)
                    .map_err(|e| ToolError::Fs(e.to_string()))?;
                let filtered: String = content
                    .lines()
                    .filter(|line| !line.contains(&item))
                    .collect::<Vec<_>>()
                    .join("\n");
                fs::write(&self.todo_path, &filtered)
                    .map_err(|e| ToolError::Fs(e.to_string()))?;
                Ok(ProgressStream::done(format!("Removed: {}", item)))
            }
            _ => Err(ToolError::InvalidArgs(format!(
                "Unknown action '{}'. Use add, list, or remove.",
                args.action
            ))),
        }
    }
}
```

### Step 2: Wire Everything Together

```rust
use deepseek::DeepSeekClient;
use engine::{Agent, AgentEvent};
use std::env;
use std::path::PathBuf;
use tokio::sync::mpsc;

mod calculator;
mod read;
mod todo;

use calculator::CalculatorTool;
use read::ReadTool;
use todo::TodoTool;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = DeepSeekClient::from_env()?;
    let workspace = env::current_dir()?;
    let todo_path = workspace.join("todo.txt").to_string_lossy().to_string();

    let agent = Agent::builder(client, "deepseek-chat")
        .system_prompt(
            "You are a personal assistant with access to a todo list, a file reader, \
             and a calculator.  Help the user manage their tasks efficiently.\n\
             When asked to do math, use the calculator tool.\n\
             When asked to manage todos, use the todo tool.\n\
             When asked to read files, use the read tool.",
        )
        .tool(CalculatorTool)
        .tool(ReadTool::new(workspace.clone()))
        .tool(TodoTool::new(todo_path))
        .build();

    // Streaming events for real-time output
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    let handle = tokio::spawn(async move {
        agent
            .run_with_events(
                "Add 'Buy groceries' to my todo list, then add 'Call dentist', \
                 and show me the full list.",
                tx,
            )
            .await
    });

    // Real-time event display
    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::Token(text) => {
                print!("{text}");
            }
            AgentEvent::ToolCall { name, arguments, .. } => {
                println!("\n🔧 {name}({arguments})");
            }
            AgentEvent::ToolSuccessful { output, .. } => {
                println!("   → {output}");
            }
            AgentEvent::Done => break,
            _ => {}
        }
    }

    let result = handle.await??;
    println!("\n\n✅ {result}");
    Ok(())
}
```

### Step 3: Run It

```bash
cargo run -p tutorial
```

You'll see the agent work through each step, calling tools and building the
solution.  The output will look something like:

```
🔧 todo({"action":"add","item":"Buy groceries"})
   → Added: Buy groceries

🔧 todo({"action":"add","item":"Call dentist"})
   → Added: Call dentist

🔧 todo({"action":"list"})
   → - [ ] Buy groceries
     - [ ] Call dentist

I've added both items to your todo list.  Here it is:

- [ ] Buy groceries
- [ ] Call dentist

✅ I've added both items to your todo list...
```

### What You Just Built

You've built a fully functional agent with:
- **Three custom tools** — calculator, file reader, todo manager
- **Streaming output** — text appears as the LLM generates it
- **Tool visualization** — you see every tool call and its result
- **ReAct loop** — the LLM autonomously decides which tools to use and in what order

This pattern scales to any tool you can imagine: web search, code execution,
database queries, image generation — the agent just needs a `Tool`
implementation.

---

## Chapter 8: Where to Go from Here

### Read the Senior Developer Guide

The [Senior Developer Guide](senior-developer-guide.md) covers everything
in depth:
- Implementing your own LLM provider (e.g., Anthropic, OpenAI)
- Manual `Tool` trait implementations for complex tools
- The full `AgentHook` lifecycle (10 callbacks)
- `EngineContext` advanced wiring
- Two-tier conversation compaction (Micro + Macro)
- WorkspaceFs sandbox internals
- Multi-layer sandbox defense (ShellFilter, ResourceTracker, AuditLogger)
- Subagent system for hierarchical delegation

### Explore the Existing Code

| What | Where | Why |
|---|---|---|
| Real tool implementations | `bins/loomis/src/tools/` | See production-quality tools |
| Sandbox system | `bins/loomis/src/sandbox/` | Full sandbox defense in depth |
| TUI implementation | `bins/loomis/src/tui/` | ratatui-based terminal UI |
| Agent assembly | `bins/loomis/src/agent_setup.rs` | How loomis wires everything |
| Compaction hooks | `extensions/compact/src/` | MicroCompact and MacroCompact |
| DeepSeek client | `core/deepseek/src/` | SSE streaming implementation |
| Engine core | `core/engine/src/agent.rs` | The ReAct loop in full detail |

### Ideas for Your Next Project

- **Code Reviewer**: Add a `grep` tool and a `shell` tool for running `cargo check`
- **Web Researcher**: Implement a `fetch_url` tool using `reqwest`
- **Database Agent**: Build a tool that runs SQL queries against SQLite
- **RAG Agent**: Add a vector search tool for document Q&A

### Key Principles to Remember

1. **Tools are the magic.**  The LLM is just reasoning — tools give it hands.
2. **System prompts matter.**  A good prompt tells the LLM *when* to use each tool.
3. **Start simple, add complexity.**  One tool at a time.  Verify it works before adding the next.
4. **Streaming is free.**  Always use `run_with_events()` for interactive apps — the user experience is dramatically better.
5. **Hooks are your observability layer.**  Logging, metrics, sandbox enforcement — hooks handle it all without touching the agent loop.

---

**Happy building!**  If you have questions, check the [Senior Developer Guide](senior-developer-guide.md)
or explore the source code in `core/`, `extensions/`, and `bins/loomis/`.
