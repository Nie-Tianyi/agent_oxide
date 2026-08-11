# agent-kit / agent-macros Usage Guide

This document explains how to use the NVIDIA OO Agents-style agent programming
paradigm in Loomis. The paradigm is provided by two new crates:

| Crate | Location | Role |
|-------|----------|------|
| **`agent-macros`** | `extensions/agent-macros/` | Proc-macros (compile time): `#[derive(Agent)]` + `#[agent_impl]`, generating all the boilerplate |
| **`agent-kit`** | `extensions/agent-kit/` | Runtime: `AgentBlueprint` trait, `run_generation`, `Strategy`, `ContextBlockHook`, `AgentAssembler` |

Design principle: **zero changes to `core/`**. The macro expansion compiles down
to existing `Tool` trait, `ToolRegistry`, `engine::Agent`, and `AgentHook` calls;
`into_agent()` produces a standard `engine::Agent<C>` that can be layered with
existing components such as `SandboxHook` and `PersistenceHook`.

Three runnable, end-to-end examples live in `extensions/agent-kit/examples/`
(`feedback_agent`, `inventory_agent`, `note_taking_agent`).

---

## 1. Quick start

### 1.1 Add dependencies

```toml
[dependencies]
agent-kit = { path = "extensions/agent-kit" }

[dependencies]            # or [dev-dependencies]
agent-macros = { path = "extensions/agent-macros" }
deepseek = { path = "core/deepseek" }
tokio = { workspace = true }
```

> All macro-generated code references core crates, serde, and schemars through
> `agent_kit::...` paths, so consumers do **not** need direct dependencies on
> `core/provider`, `core/tools`, or `core/engine`.

### 1.2 Minimal agent

```rust
use agent_kit::schemars::JsonSchema;
use agent_kit::serde::{Deserialize, Serialize};
use agent_macros::{Agent, agent_impl};
use deepseek::DeepSeekClient;

/// You are an agent specializing in analyzing customer feedback.   // ← struct doc = System Prompt
#[derive(Clone, Agent)]
struct FeedbackAgent {
    #[agent(client)]                              // ← marks the LLM client field (or just name it `client`/`llm` — auto-detected)
    client: DeepSeekClient,
}

#[agent_impl]
impl FeedbackAgent {
    /// Analyze the sentiment and key topics of customer feedback in one sentence.  // ← method doc = method prompt
    async fn analyze_feedback(&self, text: String) -> String {}
    //   ↑ empty-body async fn = generation method (implemented by the LLM)
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(crate = "agent_kit::serde")]            // key: point the derives at the re-export paths
#[schemars(crate = "agent_kit::schemars")]
struct Sentiment { label: String, score: f64 }

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = FeedbackAgent {
        client: DeepSeekClient::new(api_key),
    };

    // Generation method: await a call implemented by the LLM.
    let s = agent.analyze_feedback("Great product, but shipping was slow".into()).await?;
    println!("{s}");

    // Or assemble a full core-engine Agent (hook pipeline, ReAct loop):
    let engine_agent = agent.into_agent("deepseek-v4-pro")?;
    Ok(())
}
```

---

## 2. Reference

### 2.1 `#[derive(Agent)]` — defining an agent

Applied to a struct, it generates:

- `agent_client()` → `&C` (reference to the client field)
- `agent_model()` → `String` (value of the `#[agent(model)]` field, or `agent_kit::DEFAULT_MODEL` = `deepseek-v4-pro`)
- `agent_system_prompt()` → `String` (the struct's doc comment)
- `agent_context_prompt()` → `String` (rendered `#[context]` fields; empty string if none)
- `into_agent(model)` / `into_agent_with(model, config)` → `engine::Agent<C>` (assembly)
- the single `AgentBlueprint` trait impl (field half: system prompt, `#[tool]` field registration, context hook)

**Field attributes**:

| Attribute | Effect |
|-----------|--------|
| `#[agent(client)]` | Marks the LLM client field. Omit and name it `client`/`llm` to auto-detect |
| `#[agent(model)]` | Marks a `String` field as the default model name |
| `#[agent(skip)]` | Skips the field entirely |
| `#[tool]` / `#[tool(name = "...")]` | The field is an external tool (requires `Clone + Tool`), auto-registered |
| `#[context]` / `#[context(static)]` | Static context block: rendered once when the hook is built |
| `#[context(dynamic)]` | Dynamic context block: re-rendered before every LLM call (see §2.7) |

### 2.2 `#[agent_impl]` — processing the method block

Applied to an `impl StructName` block, it dispatches on method shape:

| Method shape | Handling |
|--------------|----------|
| `fn foo(&self, args) -> Ret { body }` (sync, with body) | The **original method is preserved** (still callable from Rust), plus a `Tool` adapter is generated and auto-registered |
| `async fn foo(&self, args) -> Ret {}` (empty body) | **Generation method**: body replaced with an LLM call, return type wrapped as `Result<Ret, agent_kit::GenerationError>` |
| `async fn foo(&self, args) -> Ret { body }` (with body) | Ordinary async method, kept as-is, no tool generated |
| `#[agent(skip)] fn ...` | Kept as-is |

### 2.3 Synchronous methods = tools

```rust
/// Get the current stock level of an item.
fn get_stock(&self, item: String) -> i32 {
    self.inventory.get(&item).map(|i| i.stock).unwrap_or(0)
}
```

- **The method signature IS the contract**: the parameter list is auto-derived
  into a `__AgentArgs_*` struct plus a JSON Schema (via `schemars`) — no manual
  Args structs.
- Tool name = method name (overridable with `#[tool(name = "...")]`),
  description = doc comment.
- Returning `Result<T, E>`: on failure, `E` is wrapped into `ToolError::Execution`
  and surfaced to the LLM.
- The return type must be `Serialize` (results are sent back to the LLM as JSON).
- The adapter holds `Arc<Struct>` — so **the agent struct must be `Clone`**.

### 2.4 Generation methods (empty-body async fns)

```rust
/// Check whether an order can be fulfilled within the budget. Return whether it
/// can be fulfilled, the total cost, and the list of out-of-stock items.
/// You MUST query real data via get_stock and get_price — never guess.
async fn can_fulfill_order(&self, items: Vec<String>, budget: f64) -> OrderResult {}
```

The macro replaces the body with a call equivalent to
([`generation.rs`](../extensions/agent-kit/src/generation.rs#L103-L124)):

1. **prompt** = method doc + `\n\nArguments:\n- items: {:?}\n- budget: {:?}`
   (parameters must be `Debug`)
2. **system** = `agent_system_prompt() + agent_context_prompt()` (context blocks inlined)
3. Execute per strategy: `Predict` = single call; `CodeAct` = register all tools
   (field tools + method tools) then run the ReAct loop
4. Return type `T`: if `DeserializeOwned + JsonSchema`, structured output
   (`response_format` + parse + retry up to `max_retries`); if `String`, raw text passthrough

Constraints: a return type is mandatory (the macro wraps it in `Result`, so do
**not** write `-> Result<T, E>`); methods cannot be generic; receivers other
than `&self` are rejected.

### 2.5 `#[strategy(...)]` — execution strategy

| Syntax | Semantics | Defaults |
|--------|-----------|----------|
| omitted | `code_act` | `max_iterations = 50`, `max_retries = 2` |
| `#[strategy(predict)]` | Single LLM call, **no tools exposed** — good for classification/extraction | `max_retries = 2` |
| `#[strategy(code_act)]` | Full ReAct loop with tools | as above |
| `#[strategy(code_act, max_iterations = 15)]` | Cap the loop iterations | — |
| `#[strategy(code_act, max_iterations = 10, max_retries = 3)]` | Configure both | — |

Maps to the runtime [`Strategy`](../extensions/agent-kit/src/generation.rs#L38-L59) enum.

### 2.6 Structured output

Enabled automatically when the return type implements `Deserialize + JsonSchema`
(the Pydantic equivalent):

```rust
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(crate = "agent_kit::serde")]
#[schemars(crate = "agent_kit::schemars")]
struct OrderResult {
    can_fulfill: bool,
    total_cost: f64,
    unavailable_items: Vec<String>,
}
```

The generation method returns `Result<OrderResult, GenerationError>`. On parse
failure the error is fed back into the next request and retried; by the time
you receive the value it is always a valid `OrderResult`.

### 2.7 `#[context(...)]` — context blocks (Python's `agent.context["notes"]`)

```rust
/// You are a note-taking agent. Your notes are provided as [CONTEXT:notes].
/// Read the current notes before answering; never invent anything not in them.
#[derive(Clone, Agent)]
struct NoteTakingAgent {
    #[agent(client)]
    client: DeepSeekClient,
    /// Dynamic context: re-rendered before every LLM call.
    #[context(dynamic)]
    notes: std::sync::Arc<std::sync::RwLock<Vec<String>>>,
}

#[agent_impl]
impl NoteTakingAgent {
    /// Add a note.
    fn add_note(&self, text: String) {
        self.notes.write().expect("lock").push(text);   // tool writes
    }

    /// Answer the user's question based on the current notes.
    async fn answer(&self, question: String) -> String {}
}
```

Mechanics:

- `static`: rendered once when the hook is built; `dynamic`: **clones the field
  and re-renders before every LLM call** (a cloned `Arc<RwLock<T>>` still points
  at the same data, so tool writes are visible to the next call).
- Full agent runs (`into_agent`) inject the block via `ContextBlockHook` as a
  `[CONTEXT:notes]`-prefixed System message (`insert_before_history`, so the
  prompt-cache prefix stays intact); generation methods bypass the hook
  pipeline, so the macro inlines `agent_context_prompt()` into the system.
- `dynamic` fields must be `Clone`. The typical shape is
  `Arc<RwLock<Vec<T>>>`, which needs serde's `rc` feature (already enabled in
  agent-kit — see §5).

### 2.8 Assembling a full core-engine Agent (`into_agent`)

`into_agent()` promotes your lightweight agent struct into a full
`core::engine::Agent<C>` — the ReAct engine that powers the TUI itself.

**Generation methods vs. the engine agent:**

| | Generation methods (`agent.classify(...)`) | `agent.into_agent(...)` → `run()` |
|---|---|---|
| LLM call | one-shot, direct | ReAct loop (tool call → observe → repeat) |
| Hook pipeline | ❌ bypassed | ✅ all hooks apply (sandbox, persistence, compaction, observability, ...) |
| Memory | none, rebuilt per call | `SharedMemory` accumulates across turns (multi-turn chat) |
| Streaming events | ❌ | ✅ `AgentEvent` (token stream, tool progress → TUI status bar) |
| Security | no sandbox | ✅ `SandboxHook` five-layer defense |
| Structured output | ✅ type-level | parsed from the returned string |

**When you need it:**

1. **Multi-turn conversation** — a generation method answers once; a chat
   needs accumulated memory:

   ```rust
   let mut agent = my_agent.clone().into_agent("deepseek-v4-pro")?;  // memory built in
   agent.run("read src/main.rs for me").await?;
   agent.run("now summarize what this file does").await?;   // remembers the previous turn
   ```

2. **The sandbox** — every tool call the LLM makes during `run()` goes through
   `SandboxHook` approval, quotas, and the audit log. Generation methods are
   unguarded.
3. **Persistence / observability** — `.loomis/threads/` auto-save, full-chain
   traces in `.loomis/logs/`, status-bar metrics — all hook pipeline features.
4. **Reuse of the existing toolset** — `read`/`shell`/`edit`/`grep` and friends
   mount on the engine's `EngineContext`; add the corresponding hooks via
   `BuildConfig` to get the same capabilities.

**Configuring hooks** — via `BuildConfig` (two equivalent styles):

```rust
// Style 1: chained builder.
let config = agent_kit::BuildConfig::default()
    .max_steps(50)
    .max_retries(3)
    .hook(SandboxHook::new(...))          // any AgentHook + 'static
    .hook(PersistenceHook::new(...))
    .streaming(true);

// Style 2: struct literal.
let config = agent_kit::BuildConfig {
    extra_hooks: vec![Box::new(MyLogHook)],
    ..Default::default()
};

let engine_agent = agent.into_agent_with("deepseek-v4-pro", config)?;
```

Notes:

- **Order = registration order.** `extra_hooks` run in push order, after the
  derive-generated `ContextBlockHook` (from `#[context]` fields). Control the
  order yourself when it matters (e.g. sandbox last) — mirror the TUI's
  registration order in `bins/loomis` (SystemPrompt → Observability →
  Persistence → Skills → PlanMode → Profile → Todo → Compact → Sandbox).
- **Generation methods bypass the hook pipeline.** `agent.classify(...)` and
  friends never touch `extra_hooks`; only the `engine::Agent` produced by
  `into_agent()` triggers them. Use `into_agent()` when you need sandbox or
  persistence.

**What it does under the hood** (`into_agent_with`, generated by the derive):

1. `ToolRegistry` ← `#[tool]` field tools + `#[agent_impl]` sync-method tools
   (via the `AgentBlueprint` halves).
2. hooks ← the `#[context]`-generated `ContextBlockHook` + `extra_hooks`
   (registration order).
3. `AgentAssembler::build()` → `EngineContext::builder(...)` → `Agent<C>`.

`BuildConfig` / `AgentAssembler` live in
[`builder.rs`](../extensions/agent-kit/src/builder.rs#L19-L155); the result is a
standard `engine::Agent<C>`, structurally identical to what the TUI's
`build_coding_agent()` produces — they coexist freely.

**Rule of thumb:** one-shot tasks that want type-safe returns → generation
methods; conversations, security, persistence → `into_agent()`.

---

## 3. Using the runtime API without macros

The macros are pure sugar; everything is callable by hand:

```rust
// Predict strategy, no tools.
let r: Sentiment = agent_kit::run_generation::<_, Sentiment>(
    agent.agent_client(),
    &agent.agent_model(),
    &format!("{}{}", agent.agent_system_prompt(), agent.agent_context_prompt()),
    "Classify the sentiment of the text.\n\nArguments:\n- text: {:?}",
    None,                                     // no tools
    &agent_kit::Strategy::Predict { max_retries: 2 },
).await?;

// CodeAct strategy with tools.
let mut reg = agent_kit::tools::ToolRegistry::new();
agent_kit::AgentBlueprint::blueprint_register_tools(&agent, &mut reg);
let out: String = agent_kit::run_generation::<_, String>(
    agent.agent_client(), &agent.agent_model(), &system, &prompt,
    Some(&reg),
    &agent_kit::Strategy::CodeAct { max_iterations: 10, max_retries: 2 },
).await?;
```

---

## 4. How it works (quick tour)

The `AgentBlueprint` trait
([`blueprint.rs`](../extensions/agent-kit/src/blueprint.rs#L24-L65)) describes
how a user-defined struct presents itself to the runtime, split into two halves:

- **field half** (generated by `#[derive(Agent)]`, the *only* trait impl):
  system prompt, `#[tool]` field registration, context hook.
- **method half** (generated by `#[agent_impl]` as same-named **inherent
  methods**): synchronous-method tool registration.

Rust forbids two `impl AgentBlueprint for T` for one type, so the derive's
trait impl calls `self.blueprint_register_method_tools(...)` (the trait's no-op
default); at the concrete `#[agent_impl]` site, inherent methods shadow the
defaults. Consequence: either macro alone compiles — an agent without
`#[agent_impl]` simply registers no method tools.

## 5. Common pitfalls

1. **`Arc<RwLock<...>>: Serialize` not satisfied** — serde 1.0.229 moved the
   `Arc`/`Rc` Serialize impls behind the optional `rc` feature. agent-kit
   enables `features = ["rc"]` in its
   [Cargo.toml](../extensions/agent-kit/Cargo.toml); if your own crate depends
   on serde directly, enable it there too.
2. **Derive paths** — structured-output types must use
   `#[serde(crate = "agent_kit::serde")]` +
   `#[schemars(crate = "agent_kit::schemars")]`, otherwise the derives resolve
   against a different serde instance than the macro-generated
   `agent_kit::serde::...` code (even at the same version, feature differences
   can surface as "two different serde versions" errors).
3. **The agent struct must be `Clone`** (tool adapters hold `Arc<Self>`).
4. **Generation-method parameters must be `Debug`** (rendered into the prompt);
   do not write `-> Result<T, E>` returns.
5. **Examples read `DEEPSEEK_API` from the environment** (they do not load
   `.env`); without the key only blueprint checks run:
   ```bash
   cargo run -p agent-kit --example inventory_agent
   ```

## 6. Verification

```bash
cargo test -p agent-kit -p agent-macros   # unit tests (context rendering, structured parsing, ...)
cargo build -p agent-kit --examples       # all three examples compile
cargo clippy -p agent-kit -p agent-macros --all-targets
```
