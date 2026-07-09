# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build                        # debug build (all crates)
cargo build --release              # release build
cargo build -p loomis              # build just the binary
cargo test --all                   # run all tests
cargo test -p provider             # run provider crate tests
cargo test -p loomis               # run loomis (binary) tests
cargo clippy --all                 # lint all crates
cargo run -p loomis                # launch the TUI
```

Set `DEEPSEEK_API` in `.env` before running — `dotenvy` loads it at startup.

## Architecture

This is a **Rust agent framework** (Rust 2024 edition, Tokio async). The project is organized as a Cargo workspace (`agent_oxide`) with six crates.

### Workspace structure

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["libs/*", "bins/*"]
├── libs/
│   ├── provider/           # LLMClient trait + shared types (Message, ToolCall, ToolDef, etc.)
│   ├── deepseek/           # DeepSeekClient — implements LLMClient with SSE streaming
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs sandbox, generate_schema
│   ├── memory/             # Memory (in-memory buffer), compaction, persistence
│   └── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent stream
├── bins/
│   └── loomis/             # Binary — concrete tools, hooks, TUI, assembly, main.rs
└── docs/
    └── architecture.md
```

### Crate map

| Crate | Location | Role | Key types |
|-------|----------|------|-----------|
| `provider` | `libs/` | Abstraction | `LLMClient` trait, `Message`, `Role`, `ToolCall`, `ToolDef`, `CompletionRequest`, `CompletionResponse`, `ProviderError`, `StreamChunk`, `Delta` |
| `deepseek` | `libs/` | Concrete | `DeepSeekClient` (impl `LLMClient`), `DeepSeekStream` (SSE parser), `DeepSeekRequest`, `DeepSeekError` |
| `tools` | `libs/` | Abstraction | `Tool` trait (sync, `Send+Sync`), `ToolRegistry`, `WorkspaceFs`, `SandboxConfig`, `ToolError`, `FsError`, `generate_schema` |
| `memory` | `libs/` | Abstraction | `Memory` (in-memory buffer), `SharedMemory` (`Arc<RwLock<Memory>>`), `MemoryBuilder`, two-tier compaction (MicroCompact + LLM summarisation), `save_conversation`/`load_conversation`/`list_threads` |
| `engine` | `libs/` | Abstraction | `Agent`, `AgentEvent` (Token, ToolCallStart, ToolResult, etc.), `AgentError`, `AgentHook` trait (on_run_start, before_tool_call, etc.), `EngineContext` (with compaction config), `maybe_compact()` |
| `loomis` | `bins/` | Binary + Lib | Concrete tools (CalculatorTool, ReadTool, ShellTool, ...), sandbox system (SandboxHook, ShellFilter, AuditLogger, ResourceTracker, EnvSanitizer), TUI (ratatui), `build_coding_agent()`, `compact_with_deepseek()`, `main.rs` |

### Dependency graph

```
provider (no internal deps)
    ↑
    ├── deepseek ──────── (implements provider::LLMClient)
    ├── tools ─────────── (uses provider::ToolDef)
    ├── memory ────────── (uses provider::Message)
    ↑
    └── engine ────────── (uses provider + tools + memory)
            ↑
        loomis (bin) ──── (uses all five libs)
```

### Key patterns

- **`LLMClient` trait** — abstraction over LLM providers. Uses `#[async_trait]` for dyn-compatibility. `DeepSeekClient` in `libs/deepseek/` is the reference implementation.
- **`Tool` trait** — sync, object-safe (no `async_trait`). CPU-bound tools run inline; I/O-heavy ones use `spawn_blocking` internally.
- **Two-tier compaction** — (1) **MicroCompact**: `compact_tool_output()` / `to_compact_context_vec()` clears old tool outputs from high-volume tools (read, shell, grep, glob, edit, write, ls), replacing content with `[Old tool result content cleared]` while keeping the most recent `keep_recent` intact. Non-mutating variant used before each LLM API call so the model sees compacted context but persistence sees full content. (2) **LLM summarisation**: `drain_for_compact()` + `apply_compact()` + `maybe_compact()`. When the character budget is exceeded, old non-System messages are drained, summarised by an LLM, and the summary is inserted as a new System message. System messages are never drained. Both tiers are configured via `EngineContext`.
- **`AgentHook` trait** — lifecycle callbacks (on_run_start, on_llm_start, on_llm_end, before_tool_call, after_tool_call). `before_tool_call` can return `Err` to block tool execution. Concrete hooks in `loomis` crate.
- **`AgentEvent` stream** — real-time tokens, tool call starts/args/results via `mpsc::unbounded_channel`. The TUI consumes these for rendering.
- **`WorkspaceFs` sandbox** — all file paths canonicalized and checked against `workspace_root`. Enforces file-size caps, extension blocklist, hidden-file protection, binary-content detection, and TOCTOU re-checks. Policy driven by [`SandboxConfig`](libs/tools/src/sandbox/config.rs) (loaded from `.loomis/config.toml`).
- **Multi-layered sandbox** — defense in depth: (1) `WorkspaceFs` path sandbox for file tools; (2) `ShellFilter` command classification (auto-approve / deny / prompt); (3) `SandboxHook` orchestrates permission checks, resource quotas, and audit logging; (4) `EnvSanitizer` clears dangerous env vars before spawning child processes.
- **Shell sandbox** — `ShellFilter` classifies commands: `auto_approve.prefixes` (e.g. `git`, `cargo`) run immediately; `deny_patterns` (e.g. `rm -rf /`, `sudo`) are blocked outright; everything else prompts the user via `SandboxHook`. Strict allowlist mode available via `allowed_commands.binaries`. Environment variables are sanitized (cleared then restored from a safe allowlist) before child processes are spawned. Watchdog kills the entire process tree on timeout (`taskkill /F /T` on Windows).
- **SandboxHook** — unified `AgentHook` replacing `DangerousCommandApprovalHook`. Checks `ResourceTracker` quotas, classifies commands via `ShellFilter`, auto-approves/denies/prompts accordingly, and logs every decision to `AuditLogger` (`.loomis/audit.jsonl`).
- **SSE streaming pipeline** — `DeepSeekStream` in `libs/deepseek/`: HTTP chunk → buffer → `find_event_end` (\n\n) → `extract_sse_data` (strip "data: ") → parse JSON → `StreamChunk`.
- **Sandbox configuration** — `.loomis/config.toml` controls all sandbox policies (filesystem limits, shell filtering, resource quotas, audit). Missing file → safe defaults. See [`SandboxConfig`](libs/tools/src/sandbox/config.rs) for the full schema.
- **`!command` shell execution** — user-typed `!` prefix in the TUI runs commands via `execute_shell_command()`, pushes output to `SharedMemory`, displays via `ShellOutput` events.
- **Conversation persistence** — saves to `.loomis/threads/{name}.json` + `.md`. Auto-save after each agent turn. `/resume` with picker overlay.

### TUI module (`bins/loomis/src/tui/`)

ratatui + crossterm chat interface. Channel topology:

```text
TUI thread                          Agent task (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

- **Keybindings**: Enter (submit), Ctrl+C (cancel), Esc (cancel), Ctrl+D (exit), PgUp/PgDown (scroll), Up/Down (history), Left/Right/Home/End (cursor)
- **Slash commands**: `/exit`, `/new`, `/save <name>`, `/resume [name]`, `/threads`, `/stats`, `/tools`, `/help`
- **Bang prefix**: `!command` runs shell, output shared with agent via SharedMemory

### Roadmap

- [x] `libs/provider` — LLMClient trait + shared types
- [x] `libs/deepseek` — DeepSeekClient with SSE streaming
- [x] `libs/tools` — Tool trait + ToolRegistry + WorkspaceFs + SandboxConfig
- [x] `libs/memory` — Memory + compaction + persistence
- [x] `libs/engine` — Agent loop + AgentHook + AgentEvent
- [x] `bins/loomis` — Concrete tools, hooks, sandbox system, TUI, assembly
- [ ] Publish lib crates to crates.io
- [ ] Add `libs/openai/`, `libs/anthropic/` provider implementations
- [ ] RAG/vector DB support (Phase 2)
- [ ] `tracing` observability (Phase 3)
- [x] Multi-layered sandbox — WorkspaceFs hardening + ShellFilter + SandboxHook + ResourceTracker + AuditLogger
- [x] Two-tier compaction — MicroCompact (tool output clearing) + LLM summarisation (`maybe_compact`)
