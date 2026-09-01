# Migration Guide: Markdown-Defined Subagents (0.6.2 → 0.7.0)

In 0.7.0 the compile-time subagent path is **removed**: subagents are now
defined exclusively as Markdown files with YAML frontmatter (`agents/*.md`,
Claude Code style), discovered at runtime. Every definition becomes its own
tool, named after the definition, with the definition's `description` as the
routing signal the parent LLM sees.

This is a **breaking change** — upgrade steps for downstream crates below.

## Summary of breaking changes

| Change | Before (0.6.2) | After (0.7.0) |
| ------ | --------------- | ------------- |
| Tool name | Fixed `"task"` | The definition's `name` |
| `SubagentTool::new` | `(llm, SubagentConfig, Arc<ToolRegistry>, SharedMemory)` | `(llm, SubagentDef, &ToolRegistry, SharedMemory, parent_model)` |
| `SubagentConfig` | Public, used by every caller | **Removed** from the public API (internal struct) |
| `filter_tools` | Public | **Removed** from the public API (internal helper) |
| `SubagentTool::from_def` | New in 0.6.3-unreleased | Renamed to `new` — the only constructor |
| Definition source | Compile-time Rust | Markdown files on disk |

**Checklist for downstream crates:**

1. Replace every `SubagentTool::new(client, config, tools, memory)` call —
   the old `SubagentConfig` literal, the manual `filter_tools` registry, and
   the fixed `"task"` tool name are all gone.
2. Write one `agents/*.md` file per subagent (see the format below) and
   discover them at startup.
3. If any code referenced the tool by name `"task"` (tool allowlists, hook
   logic, parent-tool name lists), switch it to the definition name.
4. `SubagentConfig`/`filter_tools` imports: delete. There is no replacement —
   field resolution is internal now (`SubagentDef` carries the same fields
   as `Option`s, resolved against the internal defaults).

## Why

A single compile-time `SubagentTool::new(...)` gives every delegation the
same system prompt, model, and tool set — the subagent is fixed at build
time, and adding or tuning one means recompiling the harness. Definitions
move the "what kind of subagent" decision into the LLM's hands: each
Markdown file registers as its own tool, and the parent LLM picks the right
one by its **description**, just like Claude Code's `agents/*.md`.

Because the engine advertises every registered tool's name + description to
the LLM, no system-prompt injection layer is needed — discovery is pure
filesystem work at startup, and downstream crates never touch subagent
internals again.

## Definition file format

Drop Markdown files into an `agents/` directory (or any directory you scan):

```markdown
---
name: code-reviewer
description: Review code for correctness bugs, style issues, and regressions.
model: deepseek-v4-flash
tools: [read, grep, glob]
max_steps: 40
timeout_secs: 90
inherit_context_messages: 3
---

You are a senior code reviewer. Read the relevant files first, then report
bugs by severity with file:line references. Be concise.
```

The body (after the frontmatter) becomes the subagent's **system prompt**.

### Field reference

| Field | Type | Required | Default | Notes |
| ----- | ---- | -------- | ------- | ----- |
| `name` | string | ✅ | — | Becomes the tool name. `[A-Za-z0-9_-]` only; invalid names reject the file. |
| `description` | string | ✅ | — | Becomes the tool description — **the routing signal** the parent LLM sees. Be specific about when to use this subagent. |
| `model` | string | ❌ | Parent's model | Falls back to the `parent_model` passed to `SubagentTool::new` — use the same string given to `Agent::builder`. |
| `tools` | list of strings | ❌ | `[]` (no tools) | Parent tool names the child may use. Names not present in the parent registry are dropped with a warning (typo guard). |
| `max_steps` | int | ❌ | `25` | ReAct loop iteration cap. |
| `max_retries` | int | ❌ | `2` | Retries for transient LLM failures. |
| `streaming` | bool | ❌ | `true` | SSE streaming for the child's LLM calls. |
| `timeout_secs` | int | ❌ | `120` | Hard wall-clock timeout. `0` = **no configured timeout** (the tool's built-in 300 s safety fallback still applies). |
| `inherit_context_messages` | int | ❌ | unset | Copy the last N parent messages into the child's fresh memory. |

Format rules:

- **UTF-8 without BOM.** Files must start with a `---` delimiter (leading
  blank lines are tolerated).
- **Unknown frontmatter fields are ignored** — files written against future
  versions keep loading.
- **Directory scanning is non-recursive** (`agents/*.md`, same as skills).
  Later search paths override earlier ones when names collide (project
  first, user dir second ⇒ user wins).
- Broken files (bad YAML, missing `name`/`description`, empty body) are
  **skipped with a warning**, never fatal.

## Migration: compile-time → Markdown files

### Before (0.6.2 — removed in 0.7.0)

```rust,ignore
use agent_oxide::subagent::{SubagentConfig, SubagentTool, filter_tools};

let subagent_tools = Arc::new(filter_tools(
    &parent_registry,
    &["read", "grep", "glob", "calculator"],
));

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

let agent = Agent::builder(client, model)
    .tool(subagent)
    .build();
```

### After (0.7.0 — runtime discovery)

```rust,ignore
use agent_oxide::subagent::{SubagentRegistry, register_subagents};
use std::path::PathBuf;

let registry = SubagentRegistry::discover(&[PathBuf::from("./agents")]);

let agent = register_subagents(
    Agent::builder(client.clone(), model),
    client,
    &registry,
    &parent_registry, // parent tools WITHOUT any subagent tools — see below
    parent_memory,
    model,            // fallback model for defs without `model:`
)
.build();
```

The `code-reviewer.md` equivalent of the removed `SubagentConfig` literal:

```markdown
---
name: code-reviewer
description: Review code for correctness bugs, style issues, and regressions.
model: deepseek-v4-flash
tools: [read, grep, glob, calculator]
timeout_secs: 60
---

You are a senior code reviewer. Read the relevant files first, then report
bugs by severity with file:line references. Be concise.
```

A single definition can also be wired directly, without a directory scan:

```rust,ignore
use agent_oxide::subagent::{SubagentRegistry, SubagentTool};

let registry = SubagentRegistry::discover(&[PathBuf::from("./agents")]);
let def = registry.by_name("code-reviewer").unwrap().clone();

let tool = SubagentTool::new(
    client.clone(),
    def,
    &parent_registry, // parent tools WITHOUT any subagent tools
    parent_memory,
    model,            // fallback model for defs without `model:`
);
```

## Recursion safety contract

`parent_registry` **must not contain subagent tools**. Each child's tool
set is filtered against exactly this registry, so no child can ever reach
another subagent — recursion is prevented by construction:

- Definitions whose `tools:` lists another definition's name → dropped by
  filtering (with a warning).
- Definitions whose `name` collides with an existing parent tool → **the
  definition is skipped with a warning**; your real tool is never
  clobbered.
- `register_subagents` pre-deduplicates by name (first wins), so
  discovery's later-path-overrides semantics survive `AgentBuilder::tool`
  (which is last-wins).

If you need nested delegation (a subagent that spawns its own subagent),
register the inner subagents as tools of the outer one separately — each
level gets its own registry minus the level above it.

## Error handling

`SubagentRegistry::discover` never fails: missing directories are skipped,
unreadable or unparseable files are skipped with a `tracing::warn!`, and
the registry simply holds what parsed. Check `registry.is_empty()` /
`registry.list()` if you want to surface a completely-empty result to the
user.

Parsing errors surface per-file as [`SubagentError`](`crate::subagent::SubagentError`):
`MissingOpeningDelimiter`, `MissingClosingDelimiter`, `InvalidFrontmatter`,
`InvalidToolName`, `EmptyBody`.

## API reference

| Item | Path | Purpose |
| ---- | ---- | ------- |
| `SubagentDef` | `agent_oxide::subagent::SubagentDef` | One definition parsed from `agents/*.md` |
| `SubagentRegistry` | `agent_oxide::subagent::SubagentRegistry` | `discover(&[PathBuf])`, `by_name`, `list`, `names`, `is_empty` |
| `register_subagents` | `agent_oxide::subagent::register_subagents` | Wire all defs onto an `AgentBuilder` |
| `SubagentTool::new` | `agent_oxide::subagent::SubagentTool::new` | Wire a single def directly |
| `SubagentError` | `agent_oxide::subagent::SubagentError` | Definition parse errors |

Removed (0.6.2 public API): `SubagentConfig`, `filter_tools`,
`SubagentTool::from_def` (folded into `new`), the fixed `"task"` tool name.
