#![deny(unsafe_code)]
//! # Hooks — Pluggable lifecycle behaviours for Agent Oxide
//!
//! This crate provides ready-to-use [`AgentHook`](engine::AgentHook)
//! implementations for common concerns:
//!
//! | Hook | Role |
//! |------|------|
//! | [`MicroCompactHook`] | Tool-output clearing in `on_llm_start` |
//! | [`MacroCompactHook`] | Full LLM summarisation in `on_llm_start` (blocks agent loop) |
//!
//! Both hooks operate during `on_llm_start`.  `MacroCompactHook` uses
//! [`tokio::runtime::Handle::block_on`] for the LLM call — this blocks
//! the agent task but not the TUI, since they run on different threads.
//!
//! # Custom hooks
//!
//! Implement [`engine::AgentHook`] directly for one-off behaviours.

mod compact;

pub use compact::{
    COMPACTED_TOOL_OUTPUT_PLACEHOLDER, CompactError, DEFAULT_COMPACT_CHAR_LIMIT,
    DEFAULT_COMPACT_ELIGIBLE_TOOLS, DEFAULT_COMPACT_TOKEN_LIMIT, DEFAULT_KEEP_LAST_N,
    DEFAULT_KEEP_RECENT_TOOL_OUTPUTS, MacroCompactHook, MicroCompactHook,
};
