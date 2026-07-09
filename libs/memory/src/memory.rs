//! # Memory — Conversation History with Compaction
//!
//! This module stores the agent's conversation history and provides
//! compaction mechanisms for keeping context within token-budget limits.
//!
//! ## Two-phase compaction
//!
//! 1. [`Memory::drain_for_compact`] — removes old non-System messages.
//! 2. [`Memory::apply_compact`] — inserts a summary as a new System message.
//!
//! Provider-specific compaction (e.g. `compact_with_deepseek`) lives in
//! downstream crates (e.g. `loomis`).
//!
//! ## Tool output compaction (MicroCompact)
//!
//! [`Memory::compact_tool_output`] provides a lighter-weight alternative: it
//! clears the `content` of old tool-result messages (replacing it with
//! `[Old tool result content cleared]`) while keeping the message structure
//! intact.  Only whitelisted "compactable" tool names are affected; the most
//! recent `keep_recent` outputs are preserved.  This keeps the conversation
//! context lean without losing the tool-call / tool-result pairing that the
//! model needs to understand the conversation flow.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, RwLock};

use provider::{Message, Role};

// ── Constants ─────────────────────────────────────────────────────────────────

pub const DEFAULT_COMPACT_CHARS: usize = 2_000_000;
pub const DEFAULT_KEEP_LAST_N: usize = 10;

/// Placeholder text that replaces compacted tool output content.
pub const COMPACTED_TOOL_OUTPUT_PLACEHOLDER: &str = "[Old tool result content cleared]";

/// Default number of recent tool outputs to preserve during compaction.
pub const DEFAULT_KEEP_RECENT_TOOL_OUTPUTS: usize = 5;

/// Default set of tool names whose outputs are eligible for compaction.
/// These are high-volume tools whose raw output is rarely needed in full
/// after the conversation has moved past them.
pub const DEFAULT_COMPACTABLE_TOOLS: &[&str] = &[
    "read",  // file read results
    "shell", // shell command output
    "grep",  // search results
    "glob",  // file listings
    "edit",  // edit output
    "write", // file write output
    "ls",    // directory listings
];

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum MemoryError {
    SummariserFailed(String),
    NothingToCompact,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SummariserFailed(reason) => write!(f, "summariser failed: {reason}"),
            Self::NothingToCompact => {
                write!(f, "nothing to compact — conversation is within budget")
            }
        }
    }
}

impl std::error::Error for MemoryError {}

// ── CompactSignal ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactSignal {
    WithinBudget,
    NeedsCompact,
}

// ── Memory ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Memory {
    messages: Vec<Message>,
    compact_threshold: usize,
    keep_last_n: usize,
}

pub type SharedMemory = Arc<RwLock<Memory>>;

// ── Builder ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryBuilder {
    messages: Vec<Message>,
    threshold: usize,
    keep_last: usize,
}

impl MemoryBuilder {
    pub fn threshold(mut self, chars: usize) -> Self {
        self.threshold = chars;
        self
    }

    pub fn keep_last(mut self, n: usize) -> Self {
        self.keep_last = n;
        self
    }

    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn build(self) -> Memory {
        Memory {
            messages: self.messages,
            compact_threshold: self.threshold,
            keep_last_n: self.keep_last,
        }
    }
}

// ── Construction ──────────────────────────────────────────────────────────────

impl Memory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            compact_threshold: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            messages: Vec::with_capacity(cap),
            compact_threshold: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    pub fn with_threshold(threshold: usize) -> Self {
        Self {
            messages: Vec::new(),
            compact_threshold: threshold,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    pub fn builder() -> MemoryBuilder {
        MemoryBuilder {
            messages: Vec::new(),
            threshold: DEFAULT_COMPACT_CHARS,
            keep_last: DEFAULT_KEEP_LAST_N,
        }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<Message>> for Memory {
    fn from(messages: Vec<Message>) -> Self {
        Self {
            messages,
            compact_threshold: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }
}

// ── Core Methods ──────────────────────────────────────────────────────────────

impl Memory {
    pub fn push(&mut self, message: Message) -> CompactSignal {
        self.messages.push(message);
        if self.total_chars() > self.compact_threshold {
            CompactSignal::NeedsCompact
        } else {
            CompactSignal::WithinBudget
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn to_context_vec(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Returns a copy of messages with old tool outputs compacted (content
    /// replaced by [`COMPACTED_TOOL_OUTPUT_PLACEHOLDER`]).
    ///
    /// This is the **non-mutating** variant of [`compact_tool_output`](Self::compact_tool_output).
    /// The internal messages are unchanged, so persistence still sees full
    /// content.  Use this when building the context vector for an LLM API
    /// call.
    pub fn to_compact_context_vec(
        &self,
        keep_recent: usize,
        compactable: &HashSet<String>,
    ) -> Vec<Message> {
        let mut messages = self.messages.clone();
        compact_messages(&mut messages, keep_recent, compactable);
        messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

// ── Context Length ────────────────────────────────────────────────────────────

impl Memory {
    pub fn total_chars(&self) -> usize {
        self.messages.iter().map(|m| m.content.len()).sum()
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn needs_compact(&self) -> bool {
        self.total_chars() > self.compact_threshold
    }

    pub fn compact_threshold(&self) -> usize {
        self.compact_threshold
    }

    pub fn set_compact_threshold(&mut self, threshold: usize) {
        self.compact_threshold = threshold;
    }

    pub fn keep_last_n(&self) -> usize {
        self.keep_last_n
    }
}

// ── Compaction ────────────────────────────────────────────────────────────────

impl Memory {
    /// Drains old non-System messages, keeping the most recent `keep_last_n`.
    /// System messages are never drained.
    pub fn drain_for_compact(&mut self) -> Vec<Message> {
        let non_system_count = self
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .count();
        let keep = std::cmp::min(self.keep_last_n, non_system_count);
        let to_drain = non_system_count.saturating_sub(keep);

        if to_drain == 0 {
            return Vec::new();
        }

        let mut drained = Vec::with_capacity(to_drain);
        let mut kept = Vec::with_capacity(self.messages.len() - to_drain);
        let mut drained_so_far = 0;

        for msg in self.messages.drain(..) {
            if msg.role != Role::System && drained_so_far < to_drain {
                drained.push(msg);
                drained_so_far += 1;
            } else {
                kept.push(msg);
            }
        }

        self.messages = kept;
        drained
    }

    /// Inserts a summary string as a new System message at position 0.
    pub fn apply_compact(&mut self, summary: String) {
        if summary.is_empty() {
            return;
        }
        self.messages.insert(0, Message::new(Role::System, summary));
    }
}

// ── Tool Output Compaction ──────────────────────────────────────────────────

/// Core compaction logic shared by the mutating and non-mutating variants.
///
/// Phase 1: Build `tool_call_id → tool_name` map from Assistant messages.
/// Phase 2: Walk messages in reverse, marking the most recent `keep_recent`
///          compactable tool outputs as preserved.
/// Phase 3: Replace the content of every remaining compactable tool output
///          with [`COMPACTED_TOOL_OUTPUT_PLACEHOLDER`].
///
/// Already-compacted messages (content == placeholder) are skipped — they
/// don't consume keep slots and aren't compacted again.
///
/// Returns the number of messages compacted.
fn compact_messages(
    messages: &mut [Message],
    keep_recent: usize,
    compactable: &HashSet<String>,
) -> usize {
    // Phase 1: Build tool_call_id → tool_name map from Assistant messages
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

    // Phase 2: Count compactable tool outputs from the end.
    // Already-compacted messages are skipped (they don't consume keep slots).
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

    // Phase 3: Replace content of old tool outputs with the placeholder
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

impl Memory {
    /// Compact old tool outputs in-place by replacing their content with
    /// [`COMPACTED_TOOL_OUTPUT_PLACEHOLDER`].
    ///
    /// See [`compact_messages`] for the algorithm details.
    ///
    /// Use [`to_compact_context_vec`](Self::to_compact_context_vec) if you
    /// want a compacted copy without mutating the stored messages (so that
    /// persistence can still see the full content).
    ///
    /// Returns the number of messages compacted.
    pub fn compact_tool_output(
        &mut self,
        keep_recent: usize,
        compactable: &HashSet<String>,
    ) -> usize {
        compact_messages(&mut self.messages, keep_recent, compactable)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

    #[allow(unused)]
    fn make_msg(role: Role, content: &str) -> Message {
        Message::new(role, content)
    }

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
        use provider::{ToolCall, ToolCallFunction, ToolCallType};
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

    #[test]
    fn test_new_creates_empty() {
        let mem = Memory::new();
        assert!(mem.messages().is_empty());
        assert_eq!(mem.compact_threshold(), DEFAULT_COMPACT_CHARS);
    }

    #[test]
    fn test_push_appends_message() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        assert_eq!(mem.message_count(), 1);
    }

    #[test]
    fn test_push_returns_within_budget_below_threshold() {
        let mut mem = Memory::with_threshold(1000);
        let signal = mem.push(user_msg("short"));
        assert_eq!(signal, CompactSignal::WithinBudget);
    }

    #[test]
    fn test_push_returns_needs_compact_when_over_threshold() {
        let mut mem = Memory::with_threshold(5);
        let signal = mem.push(user_msg("hello world"));
        assert_eq!(signal, CompactSignal::NeedsCompact);
    }

    #[test]
    fn test_total_chars_sums_content() {
        let mut mem = Memory::new();
        mem.push(user_msg("abc"));
        mem.push(assistant_msg("defg"));
        assert_eq!(mem.total_chars(), 7);
    }

    #[test]
    fn test_drain_preserves_last_n_messages() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 5);
        assert_eq!(mem.message_count(), 10);
        assert_eq!(mem.messages()[0].content, "msg_5");
    }

    #[test]
    fn test_drain_noop_when_fewer_than_keep() {
        let mut mem = Memory::new();
        mem.push(user_msg("a"));
        mem.push(user_msg("b"));
        let old = mem.drain_for_compact();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_apply_compact_inserts_at_front() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.apply_compact("Summary.".into());
        assert_eq!(mem.message_count(), 2);
        assert_eq!(mem.messages()[0].role, Role::System);
    }

    #[test]
    fn test_apply_compact_void_on_empty() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.apply_compact(String::new());
        assert_eq!(mem.message_count(), 1);
    }

    #[test]
    fn test_drain_preserves_system_messages() {
        let mut mem = Memory::new();
        mem.push(sys_msg("System instructions"));
        for i in 0..12 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 2);
        assert_eq!(mem.messages()[0].role, Role::System);
    }

    #[test]
    fn test_shared_memory_write_read() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        {
            let mut w = mem.write().unwrap();
            w.push(user_msg("hello"));
        }
        {
            let r = mem.read().unwrap();
            assert_eq!(r.message_count(), 1);
        }
    }

    #[test]
    fn test_builder_custom() {
        let mem = Memory::builder()
            .threshold(500)
            .keep_last(7)
            .with_messages(vec![user_msg("preloaded")])
            .build();
        assert_eq!(mem.compact_threshold(), 500);
        assert_eq!(mem.keep_last_n(), 7);
        assert_eq!(mem.message_count(), 1);
    }

    #[test]
    fn test_from_vec() {
        let msgs = vec![user_msg("a"), assistant_msg("b")];
        let mem = Memory::from(msgs);
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_memory_error_display() {
        let e = MemoryError::NothingToCompact;
        assert!(e.to_string().contains("nothing to compact"));
    }

    #[test]
    fn test_role_label_all_variants() {
        assert_eq!(role_label(Role::System), "System");
        assert_eq!(role_label(Role::User), "User");
        assert_eq!(role_label(Role::Assistant), "Assistant");
        assert_eq!(role_label(Role::Tool), "Tool");
    }

    // ── compact_tool_output tests ──────────────────────────────────────────

    #[test]
    fn test_compact_tool_output_noop_when_no_tools() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.push(assistant_msg("hi there"));
        let compacted = mem.compact_tool_output(5, &compactable_set(&["read"]));
        assert_eq!(compacted, 0);
        assert_eq!(mem.messages()[0].content, "hello");
        assert_eq!(mem.messages()[1].content, "hi there");
    }

    #[test]
    fn test_compact_tool_output_preserves_recent() {
        let mut mem = Memory::new();
        mem.push(sys_msg("system prompt"));
        // Tool call 1: read (old, should be compacted)
        mem.push(assistant_with_tool_call("call_1", "read"));
        mem.push(tool_msg("call_1", "file contents one"));
        mem.push(assistant_msg("processed file one"));
        // Tool call 2: read (should be kept)
        mem.push(assistant_with_tool_call("call_2", "read"));
        mem.push(tool_msg("call_2", "file contents two"));
        mem.push(assistant_msg("processed file two"));
        // Tool call 3: read (should be kept)
        mem.push(assistant_with_tool_call("call_3", "read"));
        mem.push(tool_msg("call_3", "file contents three"));
        mem.push(assistant_msg("processed file three"));

        let compacted = mem.compact_tool_output(2, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        // Oldest read output is compacted
        assert_eq!(mem.messages()[2].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        // Recent two are preserved
        assert_eq!(mem.messages()[5].content, "file contents two");
        assert_eq!(mem.messages()[8].content, "file contents three");
    }

    #[test]
    fn test_compact_tool_output_keep_zero_compacts_all() {
        let mut mem = Memory::new();
        mem.push(assistant_with_tool_call("call_1", "shell"));
        mem.push(tool_msg("call_1", "command output 1"));
        mem.push(assistant_with_tool_call("call_2", "shell"));
        mem.push(tool_msg("call_2", "command output 2"));

        let compacted = mem.compact_tool_output(0, &compactable_set(&["shell"]));
        assert_eq!(compacted, 2);
        assert_eq!(mem.messages()[1].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        assert_eq!(mem.messages()[3].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
    }

    #[test]
    fn test_compact_tool_output_respects_filter() {
        let mut mem = Memory::new();
        // read tool — compactable
        mem.push(assistant_with_tool_call("call_1", "read"));
        mem.push(tool_msg("call_1", "read output"));
        // calculator tool — NOT compactable
        mem.push(assistant_with_tool_call("call_2", "calculator"));
        mem.push(tool_msg("call_2", "42"));

        let compacted = mem.compact_tool_output(0, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        assert_eq!(mem.messages()[1].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        // calculator output untouched
        assert_eq!(mem.messages()[3].content, "42");
    }

    #[test]
    fn test_compact_tool_output_skips_already_compacted() {
        let mut mem = Memory::new();
        mem.push(assistant_with_tool_call("call_1", "read"));
        mem.push(tool_msg("call_1", "read output 1"));
        mem.push(assistant_with_tool_call("call_2", "read"));
        mem.push(tool_msg("call_2", "read output 2"));
        mem.push(assistant_with_tool_call("call_3", "read"));
        mem.push(tool_msg("call_3", "read output 3"));

        // First pass: keep only 1 most recent
        let c1 = mem.compact_tool_output(1, &compactable_set(&["read"]));
        assert_eq!(c1, 2);
        assert_eq!(mem.messages()[1].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        assert_eq!(mem.messages()[3].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        assert_eq!(mem.messages()[5].content, "read output 3");

        // Second pass: keep 2 most recent (but only 1 non-compacted exists)
        let c2 = mem.compact_tool_output(2, &compactable_set(&["read"]));
        assert_eq!(c2, 0); // nothing new to compact
    }

    #[test]
    fn test_compact_tool_output_keep_large_preserves_all() {
        let mut mem = Memory::new();
        mem.push(assistant_with_tool_call("call_1", "grep"));
        mem.push(tool_msg("call_1", "search results"));
        mem.push(assistant_with_tool_call("call_2", "grep"));
        mem.push(tool_msg("call_2", "more results"));

        let compacted = mem.compact_tool_output(100, &compactable_set(&["grep"]));
        assert_eq!(compacted, 0);
        assert_eq!(mem.messages()[1].content, "search results");
        assert_eq!(mem.messages()[3].content, "more results");
    }

    #[test]
    fn test_compact_tool_output_empty_compactable_set() {
        let mut mem = Memory::new();
        mem.push(assistant_with_tool_call("call_1", "read"));
        mem.push(tool_msg("call_1", "output"));

        let compacted = mem.compact_tool_output(0, &HashSet::new());
        assert_eq!(compacted, 0);
        assert_eq!(mem.messages()[1].content, "output");
    }

    #[test]
    fn test_compact_tool_output_mixed_tools() {
        let mut mem = Memory::new();
        // glob output (compactable) — old
        mem.push(assistant_with_tool_call("call_1", "glob"));
        mem.push(tool_msg("call_1", "*.rs files"));
        // grep output (compactable) — recent
        mem.push(assistant_with_tool_call("call_2", "grep"));
        mem.push(tool_msg("call_2", "found matches"));
        // shell output (compactable) — most recent
        mem.push(assistant_with_tool_call("call_3", "shell"));
        mem.push(tool_msg("call_3", "build passed"));

        let compacted = mem.compact_tool_output(2, &compactable_set(&["glob", "grep", "shell"]));
        assert_eq!(compacted, 1);
        // glob is oldest → compacted
        assert_eq!(mem.messages()[1].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        // grep and shell preserved
        assert_eq!(mem.messages()[3].content, "found matches");
        assert_eq!(mem.messages()[5].content, "build passed");
    }

    #[test]
    fn test_compact_tool_output_system_messages_untouched() {
        let mut mem = Memory::new();
        mem.push(sys_msg("system instructions"));
        mem.push(assistant_with_tool_call("call_1", "read"));
        mem.push(tool_msg("call_1", "old output"));

        let compacted = mem.compact_tool_output(0, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        // System message untouched
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "system instructions");
    }

    #[test]
    fn test_compact_tool_output_id_to_name_lookup_works() {
        let mut mem = Memory::new();
        // Assistant message with multiple tool calls
        use provider::{ToolCall, ToolCallFunction, ToolCallType};
        mem.push(Message::assistant_with_tools(
            "Let me do two things",
            vec![
                ToolCall {
                    index: 0,
                    id: "call_a".into(),
                    r#type: ToolCallType::Function,
                    function: ToolCallFunction {
                        name: "read".into(),
                        arguments: "{}".into(),
                    },
                },
                ToolCall {
                    index: 1,
                    id: "call_b".into(),
                    r#type: ToolCallType::Function,
                    function: ToolCallFunction {
                        name: "shell".into(),
                        arguments: "{}".into(),
                    },
                },
            ],
        ));
        mem.push(tool_msg("call_a", "read output"));
        mem.push(tool_msg("call_b", "shell output"));

        // read is compactable, shell is not
        let compacted = mem.compact_tool_output(0, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        assert_eq!(mem.messages()[1].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        assert_eq!(mem.messages()[2].content, "shell output");
    }

    #[test]
    fn test_compact_tool_output_preserves_most_recent_globally() {
        let mut mem = Memory::new();
        // Interleave different tools
        mem.push(assistant_with_tool_call("c1", "read"));
        mem.push(tool_msg("c1", "r1"));
        mem.push(assistant_with_tool_call("c2", "shell"));
        mem.push(tool_msg("c2", "s1"));
        mem.push(assistant_with_tool_call("c3", "read"));
        mem.push(tool_msg("c3", "r2"));
        mem.push(assistant_with_tool_call("c4", "grep"));
        mem.push(tool_msg("c4", "g1"));

        // keep 2 most recent across all compactable tools
        let compacted = mem.compact_tool_output(2, &compactable_set(&["read", "shell", "grep"]));
        assert_eq!(compacted, 2);
        // r1 and s1 are oldest → compacted
        assert_eq!(mem.messages()[1].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        assert_eq!(mem.messages()[3].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        // r2 and g1 preserved
        assert_eq!(mem.messages()[5].content, "r2");
        assert_eq!(mem.messages()[7].content, "g1");
    }

    #[test]
    fn test_default_compactable_tools_is_non_empty() {
        assert!(!DEFAULT_COMPACTABLE_TOOLS.is_empty());
    }

    #[test]
    fn test_placeholder_is_non_empty() {
        assert!(!COMPACTED_TOOL_OUTPUT_PLACEHOLDER.is_empty());
    }
}
