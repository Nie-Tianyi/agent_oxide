# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Umbrella crate** — `agent_oxide` re-exports the whole framework under
  one name (`use agent_oxide::prelude::*`), with a convenience prelude.
- **`provider`** — `LLMClient` trait (Rust 2024 native async fn, no
  `async-trait`) plus shared types: `Message`, `ToolCall`, `CompletionRequest`,
  `CompletionResponse`, `StreamChunk`, `Usage`.
- **`deepseek`** — DeepSeek API client, the reference `LLMClient`
  implementation.
- **`tools`** — `Tool` trait, `ToolRegistry`, `ProgressStream`, and the
  `#[tool]` proc macro (`tools-macros`) with lazily derived JSON Schema via
  `schemars`.
- **`memory`** — conversation memory buffer (`Memory`, `SharedMemory`,
  `PendingHints`).
- **`engine`** — the ReAct agent loop (`Agent`, `AgentBuilder`,
  `EngineContext`), the 9-callback `AgentHook` lifecycle, the streaming
  `AgentEvent` channel, and `ResponseRouter` for user-intervention routing.
- **`skills`** — `SkillDef` / `SkillRegistry`, `.md` skill discovery with
  YAML frontmatter.
- **`hooks` (compact)** — two-tier compaction: `MicroCompactHook` +
  `MacroCompactHook`.
- **`persistence`** — conversation save/load with LLM-generated thread
  titles (`PersistenceHook`).
- **`observability`** — full-chain tracing: `TraceEvent`, `TraceStore`
  (4096-entry ring buffer), lock-free `RunMetrics`.
- **`sandbox`** — 5-layer defense in depth: `WorkspaceFs` path sandbox,
  `ShellFilter` command classification, `SandboxHook` orchestration,
  `EnvSanitizer`, process watchdog.
- **`subagent`** — `SubagentTool`, spawning isolated child agents as a tool,
  with tool filtering to prevent recursion.
- **`agent_oxide::agent_kit` / `agent_oxide-macros`** — NOOA (NVIDIA OO Agents-style)
  ergonomics: `#[derive(Agent)]` + `#[agent_impl]` map class doc = system
  prompt, sync methods = tools, async methods = LLM generations,
  Pydantic-style returns = structured output, `#[strategy(...)]`,
  `#[context(...)]`, and `into_agent()` assembly into the core engine.
- **`util`** — `floor_char_boundary` helper (MSRV-compatible stand-in for
  `str::floor_char_boundary`).
- **Documentation** — README with four-layer architecture design, NOOA
  guide, Harness/UI guide, beginner guide, senior guide, sandbox
  architecture.

### Changed

- **16 crates → 2** — the workspace is now a single `agent_oxide` crate
  (all modules under `src/`) plus one proc-macro companion
  `agent-oxide-macros` (`#[derive(Agent)]`, `#[agent_impl]`, `#[tool]`).
  The old `core/` and `extensions/` sub-crates were merged; every public
  type is reachable under `agent_oxide::…` with no behavior change. The
  internal `use provider::…`-style imports became `crate::…` module paths.
- **Macro-generated paths** — `#[tool]` output references
  `::agent_oxide::tools::…` / `::agent_oxide::serde_json::…`, and
  `#[derive(Agent)]` / `#[agent_impl]` reference `agent_oxide::agent_kit::…`
  (including `agent_oxide::agent_kit::serde` / `schemars` re-exports), so
  consumers only need the two crates on crates.io.

[Unreleased]: https://github.com/Nie-Tianyi/agent_oxide/compare/master...HEAD
