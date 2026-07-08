# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build              # debug build
cargo build --release    # release build
cargo test               # run all tests
cargo test tui           # run TUI module tests only
cargo test -p agent_oxide -- test_find_event_end  # run a single test
cargo clippy             # lint
```

Set `DEEPSEEK_API` in `.env` before running — `dotenvy` loads it at startup.

## Architecture

This is a **Rust agent framework** built from scratch (Rust 2024 edition, Tokio async). The target application is an auto-researcher that autonomously uses tools to produce Markdown research reports.

### Module map

| Module | Purpose |
| ------ | ------- |
| `src/core/client/` | DeepSeek API client — typed request/response, streaming SSE support |
| `src/core/agent.rs` | Agent loop — `run_loop()` (fire-and-forget) and `run_with_events()` (real-time streaming via channel) with `max_steps` guard |
| `src/tui/` | ratatui-based chat interface — scrollable history, streaming tokens, styled tool calls, slash commands |
| `src/memory/mod.rs` | Conversation memory — `Memory`, `SharedMemory`, `MemoryBuilder`, two-phase compaction with `MemoryError` |
| `src/tools/` | Tool system — `Tool` trait, `ToolRegistry`, `ToolError`, `generate_schema` helper (schemars-based JSON Schema auto-generation), `CalculatorTool`, `EchoTool`, `ShellTool`, and file-editing tools (`ReadTool`, `WriteTool`, `EditTool`, `GlobTool`, `GrepTool`, `LsTool`) |
| `src/lib.rs` | Library crate root — re-exports `core`, `memory`, `tools`, `tui` |
| `src/main.rs` | Binary entry point — TUI by default, `--no-tui` for legacy line-based CLI |

The `core` module is the top-level crate root for the agent framework:

- **`src/core/mod.rs`** — declares `pub mod client; mod agent;`
- **`src/core/client/mod.rs`** — flat re-exports of all client types so callers `use core::client::*`

The `client` submodule is split by concern:

- **`error.rs`** — `DeepSeekError` enum (Http / Api / Parse / StreamingNotSupported)
- **`request.rs`** — `DeepSeekRequest`, `Message`, `Role`, `ToolCall`, `ToolChoice`, `ToolDef`, `FunctionDef`, `Thinking`, `ResponseFormat`, etc.
- **`response.rs`** — `DeepSeekResponse`, `FinishReason` (with `Other(String)` forward-compat), `Choice`, `ChoiceMessage`, `Usage`
- **`client.rs`** — `DeepSeekClient` — `send()` for non-streaming, `stream()` for SSE
- **`stream.rs`** — `DeepSeekStream`, `DeepSeekChunk`, `ChunkChoice`, `Delta` and the SSE parsing pipeline (3 layers: `read_event` → `extract_sse_data` → `serde_json::from_str`)

### SSE streaming pipeline

```text
HTTP chunk → buffer → find_event_end (\n\n) → trim_trailing_newlines → extract_sse_data (strip "data: ") → parse JSON → DeepSeekChunk
                                                                                         ↓
                                                                                  skip if empty / [DONE]
```

### Agent module (`src/core/agent.rs`)

| Type | Purpose |
| ---- | ------- |
| `Agent` | `{ client: DeepSeekClient, memory: SharedMemory, registry: Arc<ToolRegistry>, model, max_steps, streaming, pending_confirmations }` |
| `AgentEvent` | `Token(String)` / `ReasoningToken(String)` / `ToolCallStart { id, name }` / `ToolCallArgsDelta { id, delta }` / `ToolResult { id, name, output }` / `ConfirmShell { tool_call_id, command }` / `ShellRunning { command }` / `ShellOutput { command, output }` / `Done` — sent through `mpsc::UnboundedSender` during streaming |
| `AgentError` | `DeepSeek(String)` / `Tool { name, error }` |
| `PendingConfirmations` | `Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>` — shared state for user shell-command approval handshake |

**Core API**:

- `run_loop()` — fire-and-forget: runs the agent until `max_steps` is reached or the model returns no tool calls. Returns the final text content.
- `run_with_events(tx: UnboundedSender<AgentEvent>)` — streaming variant: sends real-time events as they arrive from the model. The caller consumes events from the receiver side.
- Builder methods: `with_model()`, `with_max_steps()`, `streaming()`, `with_pending_confirmations()`.
- **Streaming loop**: sends `Token`/`ReasoningToken` for text deltas, `ToolCallStart` when a tool call begins, `ToolCallArgsDelta` as argument chunks arrive, executes the tool after full args are assembled, sends `ToolResult`, and finally `Done`. On error, sends an error token then `Done`.

### Memory module (`src/memory/mod.rs`)

| Type | Purpose |
| ---- | ------- |
| `Memory` | `{ messages: Vec<Message>, compact_threshold: usize, keep_last_n: usize }` — configurable compaction |
| `SharedMemory` | `Arc<RwLock<Memory>>` — for sharing across tokio tasks |
| `MemoryBuilder` | Fluent builder — `.threshold(chars).keep_last(n).with_messages(vec).build()` |
| `CompactSignal` | `WithinBudget` / `NeedsCompact` — returned by `push()` |
| `MemoryError` | `SummariserFailed(String)` / `NothingToCompact` — implements `Error` + `Display` |

**Core API**: `push(msg) -> CompactSignal`, `messages()`, `to_context_vec()`, `len()`, `is_empty()`
**Context length**: `total_chars()` (sums `content` lengths), `message_count()`, `needs_compact()`, `compact_threshold()`, `set_compact_threshold()`, `keep_last_n()`
**Compaction** — two-phase design decoupling Memory from any LLM provider. **System messages are never drained** — they stay verbatim; only `User`, `Assistant`, and `Tool` messages are candidates:

```rust
// Phase 1: drain old non-System messages
let drained: Vec<Message> = mem.drain_for_compact();
// Phase 2: summarise externally (caller controls the LLM), then apply
mem.apply_compact(summary);
// Result: [System("summary..."), System(original...), msg_recent_0, ..., msg_recent_9]
```

Convenience free function `compact_with_deepseek(memory, client, model)` composes both phases against a flash model via `DeepSeekClient`. Uses `Message` and `Role` from `crate::core::client`.

### Tools module (`src/tools/`)

Split by concern, mirroring `core/client/`:

| File | Purpose |
| ---- | ------- |
| `mod.rs` | Module root — re-exports all public types (`Tool`, `ToolError`, `ToolRegistry`, `FsError`, `WorkspaceFs`, all tool structs, `generate_schema`, `tool_to_def`) |
| `error.rs` | `ToolError` — `Execution(String)` / `InvalidArgs(String)`. `FsError` — `PathEscapesWorkspace` / `NotFound` / `NotAFile` / `NotADirectory` / `Io` / `Glob` / `Regex` |
| `tool.rs` | `Tool` trait — `name()`, `description()`, `parameters()`, `execute()`, plus provided `to_def()` method |
| `registry.rs` | `ToolRegistry` — `HashMap<String, Arc<dyn Tool>>`, methods: `register`, `get`, `has`, `len`, `is_empty`, `iter`, `to_tool_defs()`, `execute()`. `tool_to_def(&dyn Tool) -> ToolDef` free function |
| `schema.rs` | `generate_schema<T: JsonSchema>() -> Value` — generates OpenAI-compatible JSON Schema from typed structs via `schemars`. Strips `$schema`, injects `additionalProperties: false`. Each tool caches its schema at construction time |
| `fs.rs` | `WorkspaceFs` — sandboxed filesystem backend. All paths canonicalized and checked against `workspace_root`. Methods: `read`, `write`, `edit_lines`, `glob`, `grep`, `ls`. Supporting types: `DirEntry`, `EntryType`, `GrepMatch` |
| `calculator.rs` | `CalculatorTool` + recursive-descent expression evaluator (`Lexer` → `Parser` separation). Supports `+`, `-`, `*`, `/`, `()`, unary `+`/`-` |
| `echo.rs` | `EchoTool` — minimal reference implementation for custom tools |
| `tool_read.rs` | `ReadTool` — reads file content with optional `offset`/`limit`, returns `cat -n` style numbered output |
| `tool_write.rs` | `WriteTool` — creates or overwrites a file, auto-creates parent directories |
| `tool_edit.rs` | `EditTool` — replaces lines `start_line..=end_line` (1-indexed) with `new_content` |
| `tool_glob.rs` | `GlobTool` — finds files matching a glob pattern, returns sorted relative paths |
| `tool_grep.rs` | `GrepTool` — regex search in files, with optional `path_glob` to filter files |
| `tool_ls.rs` | `LsTool` — lists directory contents with type/size, directories first |
| `tool_shell.rs` | `ShellTool` — executes shell commands in workspace with configurable timeout. In TUI mode, every invocation prompts the user for approval (Y/n) via an async oneshot handshake before execution |

**Tool trait** — sync, object-safe (no `async_trait` crate needed):

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;                        // auto-generated via schemars, cached
    fn execute(&self, args: &str) -> Result<String, ToolError>;
    fn to_def(&self) -> ToolDef { .. }                    // provided default
}
```

**Integration flow**:

```text
Args struct (JsonSchema + Deserialize) → generate_schema() → cached schema → parameters() clone
Tool impl → to_def() → ToolDef → DeepSeekRequest.tools
Tool impl ← ToolRegistry::execute(name, args) ← ToolCall.function.{name, arguments}
```

**Test convention**: tests live inline in each submodule file (`#[cfg(test)]` on `impl` blocks / free functions), consistent with the rest of the crate.

### TUI module (`src/tui/`)

ratatui + crossterm-based chat interface modeled after Claude Code. Split by concern:

| File | Purpose |
| ---- | ------- |
| `mod.rs` | Module root — `pub mod app; mod event; mod ui;`, re-exports `App`, `ChatMessage`, `ToolCallState`, `TuiCommand`, `run` |
| `app.rs` | Core state machine — `App` struct, `ChatMessage` enum (8 variants), `ShellOutputState`, `apply_event()` streaming state machine, `handle_key()` keyboard processing with slash commands, `!command` prefix, and Unicode-safe editing, 24 unit tests |
| `ui.rs` | ratatui rendering — three-panel `Layout` (chat/input/status), styled `Line`/`Span` per message variant, scrollable `Paragraph`, hardware cursor, line-count estimation, 5 unit tests |
| `event.rs` | Event loop + agent bridge — `run()` entry point (terminal init/restore, panic hook), `run_event_loop()` (50ms poll, agent event drain, render), `agent_handler()` async background task (spawn/cancel/clear lifecycle) |

**Channel topology** — single tokio runtime, main thread runs sync TUI loop, background `tokio::spawn` task manages agent lifecycle:

```text
TUI thread                          Agent task (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

**`ChatMessage` variants**: `User`, `Assistant`, `Reasoning`, `ToolCall { id, name, args, state }`, `System`, `ShellConfirm { tool_call_id, command, responded }`, `ShellOutput { command, state: ShellOutputState }`, `Error` — each rendered with distinct styling (see `message_to_lines()` in ui.rs). `ShellOutputState` is `Running` (showing a "Running…" indicator while a `!command` executes) or `Complete(output)`.

**`apply_event` streaming state machine**:

- `Token(t)` → append to last `Assistant` (or create new)
- `ReasoningToken(t)` → append to last `Reasoning` (or create new)
- `ToolCallStart { id, name }` → push new `ToolCall { state: Running }`
- `ToolCallArgsDelta { id, delta }` → find by id, append args
- `ToolResult { id, name, output }` → find by id, set `Complete(output)`
- `ConfirmShell { tool_call_id, command }` → push new `ShellConfirm { responded: false }`
- `ShellRunning { command }` → push `ShellOutput { state: Running }` (immediate feedback)
- `ShellOutput { command, output }` → find Running entry by command, set `state: Complete(output)`
- `Done` → set `streaming = false`

**Keybindings**: Enter (submit), Ctrl+C (cancel/exit), Esc (cancel), Ctrl+D (exit on empty), PgUp/PgDown (scroll), Up/Down (history), Left/Right/Home/End (cursor), Y/n (approve/deny shell commands). `!command` prefix runs a local shell command asynchronously, displays output inline, and shares it with the agent via SharedMemory. `!!` escapes the prefix (treated as literal text).

**Slash commands** (handled locally): `/exit`, `/clear`, `/stats`, `/tools`, `/help`. **Bang prefix** (`!<cmd>`): executes a shell command in the workspace root, shows "Running…" immediately, then displays captured stdout/stderr. Output is pushed to `SharedMemory` as a User message so the agent sees it in subsequent turns. `!!` is reserved — use it to start a message with a literal `!`.

**UTF-8 safety**: `floor_char_boundary()` used in `truncate_args()` and `truncate_output()` to avoid panics on multi-byte character boundaries.

**Entry point**: `agent_oxide::tui::run(agent, memory, tool_names, &model)` — synchronous, blocks until exit.

### Key patterns

- **Forward-compat enum**: `FinishReason::Other(String)` catches unknown values rather than failing deserialization. Custom `Serialize`/`Deserialize` because `#[serde(rename_all)]` can't handle a catch-all variant.
- **SSE event buffering**: Network chunks can split an event mid-line. The stream accumulates bytes in a `Vec<u8>` buffer and only drains when `\n\n` appears.
- **Two-phase compaction**: `drain_for_compact()` + `apply_compact()` decouples Memory from any LLM provider. The caller controls summarisation strategy; a convenience free function `compact_with_deepseek()` ties them together with a `DeepSeekClient` for the common case. System messages are never drained.
- **WorkspaceFs sandbox**: All file-editing tools hold `Arc<WorkspaceFs>` and delegate to it. `resolve()` canonicalizes every path and rejects anything outside `workspace_root`. `FsError` (I/O layer) is mapped to `ToolError` (LLM-visible) by each tool's `map_fs_err()`. The `normalize_partial()` helper handles non-existent paths by walking up to the first existing ancestor.
- **Async bridge (TUI)**: TUI event loop runs synchronously on the main thread; agent runs in a `tokio::spawn` background task. `mpsc::unbounded_channel` bridges them. `try_recv` drains agent events after each render frame; a 50ms poll timeout allows the runtime to make progress. Agent events are collected into a `Vec` and applied after `terminal.draw()` releases its immutable borrow.
- **Cancellation**: `agent_handler` tracks the current agent's `tokio::task::JoinHandle`. On `CancelGeneration`, it aborts the handle and sends a synthetic `[Cancelled]` token + `Done`. On `Exit`, it aborts and breaks the handler loop.
- **Shell confirmation handshake**: When the agent detects a `"shell"` tool call, it pauses the async loop and awaits user approval via a oneshot channel. The agent inserts a `Sender<bool>` into the shared `PendingConfirmations` map (keyed by `tool_call_id`), emits `AgentEvent::ConfirmShell` to the TUI, then awaits the receiver. The TUI renders a yellow prompt; on Y/n, it sends `TuiCommand::ShellConfirmation` back. The `agent_handler` looks up the sender and completes it, unblocking the agent. Without confirmation (CLI mode), shell commands execute unconditionally.
- **`!command` shell execution**: User-typed `!` prefix flows through `TuiCommand::RunShell` → `agent_handler` → `tokio::task::spawn_blocking(execute_shell_command)`. An immediate `AgentEvent::ShellRunning` gives the user instant feedback (yellow "Running…" in the TUI). On completion, output is decoded via UTF-8-first-then-GetACP-MultiByteToWideChar (handles Chinese GBK/CP936 on Windows pipes), pushed to `SharedMemory` as a User message, and sent to the TUI via `AgentEvent::ShellOutput`.
- **Watchdog with early-exit signal**: `Arc<AtomicBool>` lets the watchdog thread poll every 100ms and exit immediately when the command completes, rather than sleeping the full 30s timeout. `done.store(true, Relaxed)` is called after `wait_with_output()` returns; the watchdog checks the flag and returns early. Without this, `watchdog.join()` blocks for the entire timeout duration even for 15ms commands.
- **Windows pipe encoding**: cmd built-ins (`dir`, `echo`, `type`) output in the system ANSI code page (GBK/CP936 on Chinese Windows) when stdout is a pipe — `chcp 65001` does not affect pipes. The `decode_stdout()` helper in both `event.rs` and `tool_shell.rs` tries `std::str::from_utf8` first (modern CLI tools like git/cargo/rustc output valid UTF-8), then falls back to `GetACP()` + `MultiByteToWideChar()`. No external crates needed — the FFI declarations are `unsafe extern "system"` linking against kernel32.

### Roadmap (from README)

The project is in **Phase 1** (MVP). Completed and next:

- [x] `core/client/` — DeepSeek API client with typed request/response and SSE streaming
- [x] `memory/mod.rs` — conversation context with configurable compaction (`Arc<RwLock<Memory>>`), `MemoryBuilder`, two-phase `drain_for_compact`/`apply_compact`, `compact_with_deepseek` convenience fn
- [x] `tools/` — `Tool` trait + `ToolRegistry` + `CalculatorTool` + `EchoTool`, split into `mod.rs` / `error.rs` / `tool.rs` / `registry.rs` / `calculator.rs` / `echo.rs`
- [x] `tools/fs.rs` + file-editing tools — `WorkspaceFs` sandbox + `ReadTool` / `WriteTool` / `EditTool` / `GlobTool` / `GrepTool` / `LsTool`
- [x] `tools/tool_shell.rs` — `ShellTool` for executing CLI commands with timeout; TUI confirmation via async oneshot handshake (`Y`/`n`)
- [x] `core/agent.rs` — main loop with `run_loop()` (batch) and `run_with_events()` (streaming via `AgentEvent` channel), `max_steps` guard, tool-call dispatch via `ToolRegistry`
- [x] `tui/` — ratatui chat interface: scrollable history, real-time token display, styled tool calls, input with cursor/history, slash commands, `!command` shell execution with async watchdog and Windows encoding fix, status bar. Default mode; `--no-tui` for legacy CLI.
- [x] `tools/schema.rs` — `generate_schema<T: JsonSchema>()` helper for auto-generating tool JSON Schema from typed args structs (schemars integration)

Phases 2 and 3 cover streaming UX via mpsc, structured output, RAG with vector DB, and observability with `tracing`.
