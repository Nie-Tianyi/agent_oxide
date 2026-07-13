//! # Memory — Conversation History
//!
//! Stores the agent's conversation history as a plain message buffer.
//! Compaction and other policy concerns live in downstream crates (see
//! the `hooks` crate for built-in compaction strategies).

use std::sync::{Arc, RwLock};

use provider::{Message, Usage};

// ── Memory ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Memory {
    pub messages: Vec<Message>,
    /// Token usage from the most recent LLM response.
    /// `None` until the first LLM call completes.
    pub last_usage: Option<Usage>,
}

pub type SharedMemory = Arc<RwLock<Memory>>;

/// Queue for user hints injected during an active agent run.
///
/// Hints are drained into the conversation at the start of each
/// ReAct loop iteration to avoid breaking the API message-ordering
/// constraint: an assistant message with `tool_calls` must be
/// immediately followed by tool-result messages for every
/// `tool_call_id`.  No other role message may appear between them.
pub type PendingHints = Arc<std::sync::Mutex<Vec<Message>>>;

// ── Builder ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryBuilder {
    messages: Vec<Message>,
}

impl Default for MemoryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBuilder {
    /// Create a new empty [`MemoryBuilder`].
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Pre-populate the builder with existing messages.
    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn build(self) -> Memory {
        Memory {
            messages: self.messages,
            last_usage: None,
        }
    }
}

// ── Construction ──────────────────────────────────────────────────────────────

impl Memory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            last_usage: None,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            messages: Vec::with_capacity(cap),
            last_usage: None,
        }
    }

    pub fn builder() -> MemoryBuilder {
        MemoryBuilder {
            messages: Vec::new(),
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
            last_usage: None,
        }
    }
}

// ── Core Methods ──────────────────────────────────────────────────────────────

impl Memory {
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn to_context_vec(&self) -> Vec<Message> {
        self.messages.clone()
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
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use provider::Role;

    fn user_msg(content: &str) -> Message {
        Message::new(Role::User, content)
    }

    fn assistant_msg(content: &str) -> Message {
        Message::new(Role::Assistant, content)
    }

    #[test]
    fn test_new_creates_empty() {
        let mem = Memory::new();
        assert!(mem.messages().is_empty());
    }

    #[test]
    fn test_push_appends_message() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        assert_eq!(mem.len(), 1);
    }

    #[test]
    fn test_total_chars_sums_content() {
        let mut mem = Memory::new();
        mem.push(user_msg("abc"));
        mem.push(assistant_msg("defg"));
        assert_eq!(mem.total_chars(), 7);
    }

    #[test]
    fn test_shared_memory_write_read() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        {
            let mut w = mem.write().expect("memory lock poisoned");
            w.push(user_msg("hello"));
        }
        {
            let r = mem.read().expect("memory lock poisoned");
            assert_eq!(r.len(), 1);
        }
    }

    #[test]
    fn test_from_vec() {
        let msgs = vec![user_msg("a"), assistant_msg("b")];
        let mem = Memory::from(msgs);
        assert_eq!(mem.len(), 2);
    }

    #[test]
    fn test_builder_with_messages() {
        let mem = Memory::builder()
            .with_messages(vec![user_msg("preloaded")])
            .build();
        assert_eq!(mem.len(), 1);
    }

    #[test]
    fn test_len_and_is_empty() {
        let mut mem = Memory::new();
        assert!(mem.is_empty());
        assert_eq!(mem.len(), 0);
        mem.push(user_msg("a"));
        assert!(!mem.is_empty());
        assert_eq!(mem.len(), 1);
    }

    #[test]
    fn test_last_usage_defaults_to_none() {
        let mem = Memory::new();
        assert!(mem.last_usage.is_none());
    }

    #[test]
    fn test_set_last_usage() {
        let mut mem = Memory::new();
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };
        mem.last_usage = Some(usage);
        assert_eq!(mem.last_usage.as_ref().unwrap().prompt_tokens, 100);
        assert_eq!(mem.last_usage.as_ref().unwrap().completion_tokens, 50);
        assert_eq!(mem.last_usage.as_ref().unwrap().total_tokens, 150);
    }

    #[test]
    fn test_from_vec_has_last_usage_none() {
        let msgs = vec![user_msg("a")];
        let mem = Memory::from(msgs);
        assert!(mem.last_usage.is_none());
    }

    #[test]
    fn test_builder_has_last_usage_none() {
        let mem = Memory::builder().build();
        assert!(mem.last_usage.is_none());
    }
}
