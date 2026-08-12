# NOOA Step-by-Step: Defining an Agent

> **Goal:** define a complete NOOA (NVIDIA OO Agents-style) agent from
> scratch — one concept per step. You will end up with an inventory agent
> whose system prompt, tools, LLM generations, and structured outputs are
> all derived from plain Rust items.

Prerequisites: basic Rust (structs, `async`, `Arc`). The full API reference
lives in [`agent-kit-guide.md`](agent-kit-guide.md); this guide is the
walk-through that builds the code step by step.

## Step 0 — Add the dependencies

```toml
[dependencies]
agent_oxide = "0.5"
agent-oxide-macros = "0.5"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- `agent-oxide-macros` — the three proc macros: `#[derive(Agent)]` +
  `#[agent_impl]` + `#[tool]`.
- `agent_oxide` — everything else: the runtime
  (`agent_oxide::agent_kit::{AgentBlueprint, Strategy, BuildConfig}`) and the
  reference `DeepSeekClient`.

All macro-generated code resolves its paths through `agent_oxide::...`, so
you do **not** need direct dependencies on serde, schemars, or any framework
module beyond `agent_oxide` itself.

## Step 1 — Create the agent struct: the struct IS the agent

```rust,ignore
use agent_oxide::{Agent, agent_impl};

/// You are an inventory-management agent. Query real stock and prices with
/// your tools — never guess.
#[derive(Clone, Agent)]
struct InventoryAgent {
    // Fields are added in the next steps.
}
```

- The **struct doc comment becomes the system prompt**.
- `#[derive(Agent)]` generates `agent_client()`, `agent_model()`,
  `agent_system_prompt()`, `agent_context_prompt()`, and
  `into_agent()` / `into_agent_with()`.
- **`Clone` is required** — generated tool adapters hold `Arc<Self>`.

## Step 2 — Declare the LLM client

```rust,ignore
use std::collections::HashMap;

use agent_oxide::deepseek::DeepSeekClient;

#[derive(Clone, Agent)]
struct InventoryAgent {
    /// The LLM this agent talks to.
    #[agent(client)]
    client: DeepSeekClient,

    // Any other field is plain agent state — readable by your tools.
    inventory: HashMap<String, Item>,
}
```

`#[agent(client)]` marks the LLM client field; alternatively, just name the
field `client` or `llm` and it is auto-detected. Non-client fields are
ordinary state, used by your synchronous tool methods below.

## Step 3 — Add tools: synchronous methods

```rust,ignore
#[agent_impl]
impl InventoryAgent {
    /// Get the current stock level of an item.
    fn get_stock(&self, item: String) -> i32 {
        self.inventory.get(&item).map(|i| i.stock).unwrap_or(0)
    }
}
```

Inside an `#[agent_impl]` block, **a synchronous method is a tool**:

- The **method signature IS the contract** — parameters are auto-derived
  into an args struct plus a JSON Schema. No manual `Args` types.
- Tool name = method name; tool description = the doc comment.
- The return type must be `Serialize` — the result goes back to the LLM as
  JSON.
- The original method is preserved: you can still call `agent.get_stock(...)`
  directly from Rust.

## Step 4 — Add generations: async methods with an empty body

```rust,ignore
/// Check whether an order can be fulfilled within the budget: whether every
/// item is in stock, the total cost, and the out-of-stock items.
/// You MUST query real data via get_stock and get_price — never guess.
async fn can_fulfill_order(&self, items: Vec<String>, budget: f64) -> OrderResult {}
```

- An **empty-body async fn is a generation method**: the macro replaces the
  body with an LLM call.
- The method doc comment plus the rendered arguments become the prompt.
- Parameters must implement `Debug` (they are printed into the prompt).
- Do **not** write `-> Result<T, E>` — the macro wraps the return type in
  `Result<T, GenerationError>` for you.

## Step 5 — Structured output (the Pydantic equivalent)

```rust,ignore
use agent_oxide::serde::{Deserialize, Serialize};
use agent_oxide::schemars::JsonSchema;

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "agent_oxide::agent_kit::serde")]
#[schemars(crate = "agent_oxide::agent_kit::schemars")]
struct OrderResult {
    can_fulfill: bool,
    total_cost: f64,
    unavailable_items: Vec<String>,
}
```

When the return type of a generation method implements `Deserialize +
JsonSchema`, the method returns a **validated, typed value**: parse failures
are fed back into the next request and retried, so by the time you receive
the value it is always a valid `OrderResult`. The `crate = ...` attributes
are required — they make the derives resolve the same serde/schemars
instance as the macro-generated code.

## Step 6 — Choose a strategy

| Attribute | Behavior |
| --------- | -------- |
| (omitted) | `code_act` — full ReAct loop with all tools; `max_iterations = 50`, `max_retries = 2` |
| `#[strategy(predict)]` | Single LLM call, **no tools exposed** — good for classification / extraction |
| `#[strategy(code_act, max_iterations = 15)]` | ReAct loop with a step budget |
| `#[strategy(code_act, max_iterations = 15, max_retries = 3)]` | ...and a retry budget |

```rust,ignore
#[strategy(code_act, max_iterations = 15)]
async fn can_fulfill_order(&self, items: Vec<String>, budget: f64) -> OrderResult {}
```

## Step 7 — Run it

One-shot generation call — the LLM answers, the value is typed:

```rust,ignore
let result = agent
    .can_fulfill_order(vec!["apple".into(), "banana".into()], 10.0)
    .await?;
println!("{result:?}");
```

Promote to a full ReAct agent for multi-turn conversation, hooks, and
sandboxing:

```rust,ignore
let engine_agent = agent.clone().into_agent("deepseek-v4-pro")?;
let answer = engine_agent.run("What is 2+2?").await?;
println!("{answer}");
```

## The complete agent

```rust,ignore
use std::collections::HashMap;

use agent_oxide::schemars::JsonSchema;
use agent_oxide::serde::{Deserialize, Serialize};
use agent_oxide::{Agent, agent_impl};
use agent_oxide::deepseek::DeepSeekClient;

/// You are an inventory-management agent. Query real stock and prices with
/// your tools — never guess.
#[derive(Clone, Agent)]
struct InventoryAgent {
    #[agent(client)]
    client: DeepSeekClient,
    inventory: HashMap<String, Item>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "agent_oxide::agent_kit::serde")]
#[schemars(crate = "agent_oxide::agent_kit::schemars")]
struct Item {
    name: String,
    stock: i32,
    price: f64,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "agent_oxide::agent_kit::serde")]
#[schemars(crate = "agent_oxide::agent_kit::schemars")]
struct OrderResult {
    can_fulfill: bool,
    total_cost: f64,
    unavailable_items: Vec<String>,
}

#[agent_impl]
impl InventoryAgent {
    /// Get the current stock level of an item.
    fn get_stock(&self, item: String) -> i32 {
        self.inventory.get(&item).map(|i| i.stock).unwrap_or(0)
    }

    /// Get the current unit price of an item.
    fn get_price(&self, item: String) -> f64 {
        self.inventory.get(&item).map(|i| i.price).unwrap_or(0.0)
    }

    /// Check whether an order can be fulfilled within the budget: whether
    /// every item is in stock, the total cost, and the out-of-stock items.
    /// You MUST query real data via get_stock and get_price — never guess.
    #[strategy(code_act, max_iterations = 15)]
    async fn can_fulfill_order(&self, items: Vec<String>, budget: f64) -> OrderResult {}
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let inventory = HashMap::from([
        ("apple".into(), Item { name: "apple".into(), stock: 5, price: 3.0 }),
        ("banana".into(), Item { name: "banana".into(), stock: 0, price: 1.0 }),
    ]);

    let agent = InventoryAgent {
        client: DeepSeekClient::new(std::env::var("DEEPSEEK_API")?),
        inventory,
    };

    // One-shot: the LLM must call get_stock/get_price, then produce a typed
    // OrderResult — banana is out of stock, so can_fulfill = false.
    let result = agent
        .can_fulfill_order(vec!["apple".into(), "banana".into()], 10.0)
        .await?;
    println!("{result:?}");

    // Multi-turn: promote to a full ReAct agent (memory + hooks).
    let engine_agent = agent.clone().into_agent("deepseek-v4-pro")?;
    let answer = engine_agent.run("What is 2+2?").await?;
    println!("{answer}");
    Ok(())
}
```

## What's next

- [`agent-kit-guide.md`](agent-kit-guide.md) — the full reference: field
  attributes, `#[context(...)]` blocks, `BuildConfig` hook wiring, the
  macro-free runtime API, and common pitfalls.
- [`harness-ui-guide.md`](harness-ui-guide.md) — build your own Harness and
  UI layers on top of the core APIs when the macros are not enough.
- `README.md` — the NOOA concept table and the four-layer architecture.
