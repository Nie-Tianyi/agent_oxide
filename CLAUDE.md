# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build              # debug build
cargo build --release    # release build
cargo test               # run all tests
cargo test -p agent_oxide -- test_find_event_end  # run a single test
cargo clippy             # lint
```

Set `DEEPSEEK_API` in `.env` before running — `dotenvy` loads it at startup.

## Architecture

This is a **Rust agent framework** built from scratch (Rust 2024 edition, Tokio async). The target application is an auto-researcher that autonomously uses tools to produce Markdown research reports.

### Current phase (MVP)

| Module | Purpose |
| ------ | ------- |
| `src/core/client/` | DeepSeek API client — typed request/response, streaming SSE support |
| `src/core/agent.rs` | Agent scaffolding (empty — next to implement) |
| `src/lib.rs` | Library crate root — re-exports `core` and `memory` for doc tests and external consumers |
| `src/memory/mod.rs` | Conversation memory — `Memory`, `SharedMemory`, `MemoryBuilder`, two-phase compaction with `MemoryError` |
| `src/main.rs` | Scratchpad: raw HTTP-level SSE demo (does **not** use the `client` module yet) |

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

### Key patterns

- **Forward-compat enum**: `FinishReason::Other(String)` catches unknown values rather than failing deserialization. Custom `Serialize`/`Deserialize` because `#[serde(rename_all)]` can't handle a catch-all variant.
- **SSE event buffering**: Network chunks can split an event mid-line. The stream accumulates bytes in a `Vec<u8>` buffer and only drains when `\n\n` appears.
- **Two-phase compaction**: `drain_for_compact()` + `apply_compact()` decouples Memory from any LLM provider. The caller controls summarisation strategy; a convenience free function `compact_with_deepseek()` ties them together with a `DeepSeekClient` for the common case. System messages are never drained.

### Roadmap (from README)

The project is in **Phase 1** (MVP). Completed and next:

- [x] `core/client/` — DeepSeek API client with typed request/response and SSE streaming
- [x] `memory/mod.rs` — conversation context with configurable compaction (`Arc<RwLock<Memory>>`), `MemoryBuilder`, two-phase `drain_for_compact`/`apply_compact`, `compact_with_deepseek` convenience fn
- [ ] `tools.rs` — `Tool` trait + tool registry + 1–2 example tools
- [ ] `core/agent.rs` — main loop: LLM → match (text → done / tool_calls → execute → push to memory → loop), with `max_steps` guard (scaffolding exists)

Phases 2 and 3 cover macros/schemars for auto-schema, streaming UX via mpsc, structured output, RAG with vector DB, TUI, and observability with `tracing`.
