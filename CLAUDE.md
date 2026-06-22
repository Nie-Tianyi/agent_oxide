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

Set `DeepSeek_API` in `.env` before running — `dotenvy` loads it at startup.

## Architecture

This is a **Rust agent framework** built from scratch (Rust 2024 edition, Tokio async). The target application is an auto-researcher that autonomously uses tools to produce Markdown research reports.

### Current phase (MVP)

| Module | Purpose |
|--------|---------|
| `src/client/` | DeepSeek API client — typed request/response, streaming SSE support |
| `src/main.rs` | Scratchpad: currently a raw HTTP-level SSE demo (does **not** use the `client` module yet) |

The `client` module is split by concern:

- **`error.rs`** — `DeepSeekError` enum (Http / Api / Parse / StreamingNotSupported)
- **`request.rs`** — `DeepSeekRequest`, `Message`, `Role`, `ToolCall`, `ToolChoice`, `ToolDef`, etc.
- **`response.rs`** — `DeepSeekResponse`, `FinishReason` (with `Other(String)` forward-compat), `Choice`, `Usage`
- **`client.rs`** — `DeepSeekClient` — `send()` for non-streaming, `stream()` for SSE
- **`stream.rs`** — `DeepSeekStream` and the SSE parsing pipeline (3 layers: `read_event` → `extract_sse_data` → `serde_json::from_str`)
- **`mod.rs`** — flat re-exports so callers `use client::*`

### SSE streaming pipeline

```
HTTP chunk → buffer → find_event_end (\n\n) → trim_trailing_newlines → extract_sse_data (strip "data: ") → parse JSON → DeepSeekChunk
                                                                                          ↓
                                                                                   skip if empty / [DONE]
```

### Key patterns

- **Forward-compat enum**: `FinishReason::Other(String)` catches unknown values rather than failing deserialization. Custom `Serialize`/`Deserialize` because `#[serde(rename_all)]` can't handle a catch-all variant.
- **SSE event buffering**: Network chunks can split an event mid-line. The stream accumulates bytes in a `Vec<u8>` buffer and only drains when `\n\n` appears.

### Roadmap (from README)

The project is in **Phase 1** (MVP). Next items to build:
1. `memory.rs` — conversation context with sliding window truncation (`Arc<RwLock<Memory>>`)
2. `tools.rs` — `Tool` trait + tool registry + 1–2 example tools
3. `core/agent.rs` — main loop: LLM → match (text → done / tool_calls → execute → push to memory → loop), with `max_steps` guard

Phases 2 and 3 cover macros/schemars for auto-schema, streaming UX via mpsc, structured output, RAG with vector DB, TUI, and observability with `tracing`.
