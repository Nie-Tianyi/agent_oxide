//! # Compaction
//!
//! Two-tier context compaction:
//!
//! - [`MicroCompactHook`] — an [`AgentHook`] that clears old tool-output
//!   content in-place during `on_llm_start`.
//!
//! - [`MacroCompactConfig`] — configuration for full LLM summarisation.
//!   The agent loop calls out to a cheap model when the character budget
//!   is exceeded, draining old non-System messages and inserting a summary
//!   as a new System message.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use engine::AgentHook;
use memory::SharedMemory;
use provider::{CompletionRequest, LLMClient, Message, Role};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Placeholder text that replaces compacted tool output content.
pub const COMPACTED_TOOL_OUTPUT_PLACEHOLDER: &str = "[Old tool result content cleared]";

/// Default number of recent tool outputs to preserve during compaction.
pub const DEFAULT_KEEP_RECENT_TOOL_OUTPUTS: usize = 5;

/// Default set of tool names whose outputs are eligible for compaction.
pub const DEFAULT_COMPACT_ELIGIBLE_TOOLS: &[&str] =
    &["read", "shell", "grep", "glob", "edit", "write", "ls"];

/// Default character budget before macro-compaction triggers.
pub const DEFAULT_COMPACT_CHAR_LIMIT: usize = 2_000_000;

/// Default number of non-System messages preserved during macro-compaction drain.
pub const DEFAULT_KEEP_LAST_N: usize = 10;

// ── CompactError ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum CompactError {
    /// The summarisation model returned an error.
    SummariserFailed(String),
}

impl fmt::Display for CompactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SummariserFailed(reason) => write!(f, "summariser failed: {reason}"),
        }
    }
}

impl std::error::Error for CompactError {}

// ── MicroCompactHook ──────────────────────────────────────────────────────────

/// Lightweight tool-output compaction hook.
///
/// Implements [`AgentHook`] — in `on_llm_start`, clears old tool-result
/// content in-place, replacing it with `[Old tool result content cleared]`.
/// The most recent `keep_recent` outputs per compactable tool are preserved.
pub struct MicroCompactHook {
    /// How many of the most recent tool outputs to preserve.
    pub keep_recent: usize,
    /// Which tool names are eligible for output compaction.
    pub compact_eligible_tools: HashSet<String>,
}

impl MicroCompactHook {
    pub fn new(keep_recent: usize, compact_eligible_tools: HashSet<String>) -> Self {
        Self {
            keep_recent,
            compact_eligible_tools,
        }
    }
}

impl AgentHook for MicroCompactHook {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let mut mem = memory.write().expect("memory lock poisoned");
        compact_messages(
            &mut mem.messages,
            self.keep_recent,
            &self.compact_eligible_tools,
        );
    }
}

// ── MacroCompactHook ──────────────────────────────────────────────────────────

/// Full LLM summarisation hook.
///
/// Implements [`AgentHook`] — in `on_llm_start`, checks whether the conversation
/// exceeds `threshold` characters.  If it does, drains old non-System messages
/// (keeping the most recent `keep_last_n`), calls the compact model for a
/// summary, and inserts it as a System message.
///
/// The LLM call blocks the agent loop via
/// [`tokio::runtime::Handle::block_on`].  This is safe because the agent loop
/// runs in a dedicated tokio task, separate from the TUI main thread — blocking
/// here does not affect the UI.
pub struct MacroCompactHook<C: LLMClient> {
    /// Model name for summarisation (cheap model).
    pub compact_model: String,
    /// Character budget before compaction triggers.
    pub threshold: usize,
    /// Number of non-System messages to preserve during drain.
    pub keep_last_n: usize,
    /// LLM client (same provider, different model).
    pub client: C,
    /// Set when a summarisation attempt fails — prevents retrying on every
    /// subsequent LLM call until memory grows further or compaction succeeds.
    pub compaction_failed: AtomicBool,
}

impl<C: LLMClient> MacroCompactHook<C> {
    pub fn new(compact_model: String, threshold: usize, keep_last_n: usize, client: C) -> Self {
        Self {
            compact_model,
            threshold,
            keep_last_n,
            client,
            compaction_failed: AtomicBool::new(false),
        }
    }
}

impl<C: LLMClient> AgentHook for MacroCompactHook<C> {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let needs = {
            let mem = memory.read().expect("memory lock poisoned");
            mem.total_chars() > self.threshold
        };
        if !needs {
            return;
        }

        let old = {
            let mut mem = memory.write().expect("memory lock poisoned");
            drain_for_compact(&mut mem.messages, self.keep_last_n)
        };
        if old.is_empty() {
            return;
        }

        // Build summarisation transcript
        let transcript: String = old
            .iter()
            .map(|m| format!("[{}]: {}", role_label(m.role), m.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "Summarise the following conversation history concisely. \
             Preserve key facts, decisions, and context. \
             Output only the summary, no preamble:\n\n{transcript}"
        );

        let request =
            CompletionRequest::new(&self.compact_model, vec![Message::new(Role::User, prompt)]);

        // Block the agent loop (not the UI — different thread).
        let result = tokio::runtime::Handle::current().block_on(self.client.generate(request));

        let summary = match result {
            Ok(resp) => {
                // Summarisation succeeded — clear the failure flag.
                self.compaction_failed.store(false, Ordering::Relaxed);
                resp.choices
                    .into_iter()
                    .next()
                    .and_then(|c| c.message.content)
                    .unwrap_or_default()
            }
            Err(e) => {
                // Summarisation failed — log the error and set a flag to
                // avoid retrying on every subsequent LLM call (which would
                // burn API calls in a tight loop).
                if !self.compaction_failed.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "WARNING: Macro-compaction summarisation failed: {e}\n  \
                         Will not retry compaction until it succeeds once."
                    );
                }
                String::new()
            }
        };

        if !summary.is_empty() {
            let mut mem = memory.write().expect("memory lock poisoned");
            mem.messages.insert(0, Message::new(Role::System, summary));
        }
    }
}

// ── Tool Output Compaction (core algorithm) ───────────────────────────────────

fn compact_messages(
    messages: &mut [Message],
    keep_recent: usize,
    compactable: &HashSet<String>,
) -> usize {
    let mut id_to_name: HashMap<String, String> = HashMap::new();
    for msg in messages.iter() {
        if msg.role == Role::Assistant
            && let Some(ref tool_calls) = msg.tool_calls
        {
            for tc in tool_calls {
                id_to_name.insert(tc.id.clone(), tc.function.name.clone());
            }
        }
    }

    if id_to_name.is_empty() {
        return 0;
    }

    let mut compactable_count_from_end = 0usize;
    let mut should_keep = vec![false; messages.len()];

    for (i, msg) in messages.iter().enumerate().rev() {
        if msg.role != Role::Tool {
            continue;
        }
        if msg.content == COMPACTED_TOOL_OUTPUT_PLACEHOLDER {
            continue;
        }
        if let Some(ref tool_call_id) = msg.tool_call_id
            && let Some(tool_name) = id_to_name.get(tool_call_id)
            && compactable.contains(tool_name)
            && compactable_count_from_end < keep_recent
        {
            should_keep[i] = true;
            compactable_count_from_end += 1;
        }
    }

    let mut compacted = 0usize;
    for (i, msg) in messages.iter_mut().enumerate() {
        if msg.role != Role::Tool || should_keep[i] {
            continue;
        }
        if msg.content == COMPACTED_TOOL_OUTPUT_PLACEHOLDER {
            continue;
        }
        if let Some(ref tool_call_id) = msg.tool_call_id
            && let Some(tool_name) = id_to_name.get(tool_call_id)
            && compactable.contains(tool_name)
        {
            msg.content = COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string();
            compacted += 1;
        }
    }

    compacted
}

// ── Private helpers (also used by tests) ──────────────────────────────────────

fn drain_for_compact(messages: &mut Vec<Message>, keep_last_n: usize) -> Vec<Message> {
    let non_system_count = messages.iter().filter(|m| m.role != Role::System).count();
    let keep = std::cmp::min(keep_last_n, non_system_count);
    let to_drain = non_system_count.saturating_sub(keep);
    if to_drain == 0 {
        return Vec::new();
    }
    let mut drained = Vec::with_capacity(to_drain);
    let mut kept = Vec::with_capacity(messages.len() - to_drain);
    let mut drained_so_far = 0;
    for msg in messages.drain(..) {
        if msg.role != Role::System && drained_so_far < to_drain {
            drained.push(msg);
            drained_so_far += 1;
        } else {
            kept.push(msg);
        }
    }
    *messages = kept;
    drained
}

const fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
        _ => "Unknown",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use provider::{ToolCall, ToolCallFunction, ToolCallKind};

    fn user_msg(content: &str) -> Message {
        Message::new(Role::User, content)
    }

    fn assistant_msg(content: &str) -> Message {
        Message::new(Role::Assistant, content)
    }

    fn sys_msg(content: &str) -> Message {
        Message::new(Role::System, content)
    }

    fn assistant_with_tool_call(id: &str, tool_name: &str) -> Message {
        Message::assistant_with_tools(
            "",
            vec![ToolCall {
                index: 0,
                id: id.to_string(),
                kind: ToolCallKind::Function,
                function: ToolCallFunction {
                    name: tool_name.to_string(),
                    arguments: "{}".to_string(),
                },
            }],
        )
    }

    fn tool_msg(id: &str, content: &str) -> Message {
        Message::tool_result(id, content)
    }

    fn compactable_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ── compact_messages tests ─────────────────────────────────────────────

    #[test]
    fn test_compact_tool_output_noop_when_no_tools() {
        let mut messages = vec![user_msg("hello"), assistant_msg("hi there")];
        let compacted = compact_messages(&mut messages, 5, &compactable_set(&["read"]));
        assert_eq!(compacted, 0);
    }

    #[test]
    fn test_compact_tool_output_preserves_recent() {
        let mut messages = vec![
            sys_msg("system prompt"),
            assistant_with_tool_call("call_1", "read"),
            tool_msg("call_1", "file contents one"),
            assistant_msg("processed file one"),
            assistant_with_tool_call("call_2", "read"),
            tool_msg("call_2", "file contents two"),
            assistant_msg("processed file two"),
            assistant_with_tool_call("call_3", "read"),
            tool_msg("call_3", "file contents three"),
            assistant_msg("processed file three"),
        ];
        let compacted = compact_messages(&mut messages, 2, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        assert_eq!(messages[2].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        assert_eq!(messages[5].content, "file contents two");
        assert_eq!(messages[8].content, "file contents three");
    }

    #[test]
    fn test_compact_tool_output_keep_zero_compacts_all() {
        let mut messages = vec![
            assistant_with_tool_call("call_1", "shell"),
            tool_msg("call_1", "command output 1"),
            assistant_with_tool_call("call_2", "shell"),
            tool_msg("call_2", "command output 2"),
        ];
        let compacted = compact_messages(&mut messages, 0, &compactable_set(&["shell"]));
        assert_eq!(compacted, 2);
    }

    #[test]
    fn test_compact_tool_output_respects_filter() {
        let mut messages = vec![
            assistant_with_tool_call("call_1", "read"),
            tool_msg("call_1", "read output"),
            assistant_with_tool_call("call_2", "calculator"),
            tool_msg("call_2", "42"),
        ];
        let compacted = compact_messages(&mut messages, 0, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        assert_eq!(messages[3].content, "42");
    }

    #[test]
    fn test_compact_tool_output_skips_already_compacted() {
        let mut messages = vec![
            assistant_with_tool_call("call_1", "read"),
            tool_msg("call_1", "read output 1"),
            assistant_with_tool_call("call_2", "read"),
            tool_msg("call_2", "read output 2"),
            assistant_with_tool_call("call_3", "read"),
            tool_msg("call_3", "read output 3"),
        ];
        let c1 = compact_messages(&mut messages, 1, &compactable_set(&["read"]));
        assert_eq!(c1, 2);
        let c2 = compact_messages(&mut messages, 2, &compactable_set(&["read"]));
        assert_eq!(c2, 0);
    }

    #[test]
    fn test_compact_tool_output_empty_compactable_set() {
        let mut messages = vec![
            assistant_with_tool_call("call_1", "read"),
            tool_msg("call_1", "output"),
        ];
        let compacted = compact_messages(&mut messages, 0, &HashSet::new());
        assert_eq!(compacted, 0);
    }

    #[test]
    fn test_default_compact_eligible_tools_is_non_empty() {
        assert!(!DEFAULT_COMPACT_ELIGIBLE_TOOLS.is_empty());
    }

    #[test]
    fn test_placeholder_is_non_empty() {
        assert!(!COMPACTED_TOOL_OUTPUT_PLACEHOLDER.is_empty());
    }

    // ── drain_for_compact tests ────────────────────────────────────────────

    #[test]
    fn test_drain_preserves_last_n_messages() {
        let mut messages: Vec<Message> = (0..15).map(|i| user_msg(&format!("msg_{i}"))).collect();
        let initial_len = messages.len();
        let old = drain_for_compact(&mut messages, 10);
        assert_eq!(old.len(), initial_len - 10);
        assert_eq!(messages.len(), 10);
    }

    #[test]
    fn test_drain_noop_when_fewer_than_keep() {
        let mut messages = vec![user_msg("a"), user_msg("b")];
        let old = drain_for_compact(&mut messages, 10);
        assert!(old.is_empty());
    }

    #[test]
    fn test_drain_preserves_system_messages() {
        let mut messages = vec![sys_msg("System instructions")];
        for i in 0..12 {
            messages.push(user_msg(&format!("msg_{i}")));
        }
        let old = drain_for_compact(&mut messages, 10);
        assert_eq!(old.len(), 2);
        assert_eq!(messages[0].role, Role::System);
    }

    #[test]
    fn test_role_label_all_variants() {
        assert_eq!(role_label(Role::System), "System");
        assert_eq!(role_label(Role::User), "User");
        assert_eq!(role_label(Role::Assistant), "Assistant");
        assert_eq!(role_label(Role::Tool), "Tool");
    }

    #[test]
    fn test_compact_error_display() {
        assert!(
            CompactError::SummariserFailed("test".into())
                .to_string()
                .contains("summariser failed")
        );
    }
}
