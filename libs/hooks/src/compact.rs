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

use engine::AgentHook;
use memory::SharedMemory;
use provider::{Message, Role};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Placeholder text that replaces compacted tool output content.
pub const COMPACTED_TOOL_OUTPUT_PLACEHOLDER: &str = "[Old tool result content cleared]";

/// Default number of recent tool outputs to preserve during compaction.
pub const DEFAULT_KEEP_RECENT_TOOL_OUTPUTS: usize = 5;

/// Default set of tool names whose outputs are eligible for compaction.
pub const DEFAULT_COMPACTABLE_TOOLS: &[&str] =
    &["read", "shell", "grep", "glob", "edit", "write", "ls"];

/// Default character budget before macro-compaction triggers.
pub const DEFAULT_COMPACT_CHARS: usize = 2_000_000;

/// Default number of non-System messages preserved during macro-compaction drain.
pub const DEFAULT_KEEP_LAST_N: usize = 10;

// ── CompactError ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum CompactError {
    SummariserFailed(String),
    NothingToCompact,
}

impl fmt::Display for CompactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SummariserFailed(reason) => write!(f, "summariser failed: {reason}"),
            Self::NothingToCompact => {
                write!(f, "nothing to compact — conversation is within budget")
            }
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
    pub compactable_tools: HashSet<String>,
}

impl MicroCompactHook {
    pub fn new(keep_recent: usize, compactable_tools: HashSet<String>) -> Self {
        Self {
            keep_recent,
            compactable_tools,
        }
    }
}

impl AgentHook for MicroCompactHook {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let mut mem = memory.write().unwrap();
        compact_messages(&mut mem.messages, self.keep_recent, &self.compactable_tools);
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

#[allow(dead_code)]
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

#[allow(dead_code)]
const fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use provider::{ToolCall, ToolCallFunction, ToolCallType};

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
                r#type: ToolCallType::Function,
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
    fn test_default_compactable_tools_is_non_empty() {
        assert!(!DEFAULT_COMPACTABLE_TOOLS.is_empty());
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
            CompactError::NothingToCompact
                .to_string()
                .contains("nothing to compact")
        );
    }
}
