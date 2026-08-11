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
//! [`engine::block_on`] for the LLM call — this blocks the agent task
//! but not the TUI, since they run on different threads.
//!
//! # Custom hooks
//!
//! Implement [`engine::AgentHook`] directly for one-off behaviours.
//!
//! # Shared helpers
//!
//! [`insert_before_history`] is the canonical way for injector hooks to place
//! a System message without breaking the provider's prompt-cache prefix.

use provider::{Message, Role};

mod compact;

/// Insert `msg` after the trailing System-message block — i.e. before the
/// first non-System message, or at the end if there are none.
///
/// Injector hooks must use this instead of `insert(0, …)`: volatile injected
/// content ([SKILL], [PLAN_MODE], [PROFILE], [COMPACT_SUMMARY], [TODO])
/// placed in front of the static system prompt would invalidate the
/// provider's prompt-cache prefix on every change, forcing a full re-parse of
/// the most expensive (and most stable) part of the request.  Anchoring the
/// insert at the tail of the System block keeps the static head
/// byte-identical, so only the dynamic history misses.
///
/// The hook registration order decides the ordering inside the tail: the more
/// volatile an injector, the later it must run, so it lands closer to the
/// history.  `TodoListHook` is the most volatile ([TODO] changes on every
/// plan update) and runs last, keeping the rarely-changing
/// `[COMPACT_SUMMARY]` before it — a plan update then only invalidates the
/// cache prefix past the history boundary, never the compaction summary.
pub fn insert_before_history(messages: &mut Vec<Message>, msg: Message) {
    let idx = messages
        .iter()
        .position(|m| m.role != Role::System)
        .unwrap_or(messages.len());
    messages.insert(idx, msg);
}

pub use compact::{
    COMPACT_SUMMARY_MARKER, COMPACTED_TOOL_OUTPUT_PLACEHOLDER, COMPACTED_TOOL_OUTPUT_PREFIX,
    CompactError, DEFAULT_COMPACT_CHAR_LIMIT, DEFAULT_COMPACT_ELIGIBLE_TOOLS,
    DEFAULT_COMPACT_TOKEN_LIMIT, DEFAULT_KEEP_LAST_N, DEFAULT_KEEP_RECENT_TOOL_OUTPUTS,
    MacroCompactHook, MicroCompactHook,
};
