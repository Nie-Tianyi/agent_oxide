//! # Hooks — Pluggable lifecycle behaviours for Agent Oxide
//!
//! This crate provides ready-to-use [`AgentHook`](engine::AgentHook)
//! implementations for common concerns:
//!
//! | Component | Role |
//! |-----------|------|
//! | [`MicroCompactHook`] | AgentHook — tool-output clearing in `on_llm_start` |
//!
//! For macro-compaction (LLM summarisation), use
//! [`engine::MacroCompactConfig`] — the agent loop handles the async
//! summarisation directly.  This crate provides the default constants
//! (`DEFAULT_COMPACT_CHARS`, `DEFAULT_KEEP_LAST_N`, etc.).
//!
//! # Custom hooks
//!
//! Implement [`engine::AgentHook`] directly for one-off behaviours.

mod compact;

pub use compact::{
    COMPACTED_TOOL_OUTPUT_PLACEHOLDER, CompactError, DEFAULT_COMPACT_CHARS,
    DEFAULT_COMPACTABLE_TOOLS, DEFAULT_KEEP_LAST_N, DEFAULT_KEEP_RECENT_TOOL_OUTPUTS,
    MicroCompactHook,
};
