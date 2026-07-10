//! # Memory — Conversation History
//!
//! Stores the agent's conversation history as a plain message buffer.
//! Compaction and other policy concerns live in downstream crates (see
//! the `hooks` crate for built-in compaction strategies).

use std::sync::{Arc, RwLock};

use provider::Message;

// ── Memory ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Memory {
    pub messages: Vec<Message>,
}

pub type SharedMemory = Arc<RwLock<Memory>>;

// ── Builder ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryBuilder {
    messages: Vec<Message>,
}

impl MemoryBuilder {
    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn build(self) -> Memory {
        Memory {
            messages: self.messages,
        }
    }
}

// ── Construction ──────────────────────────────────────────────────────────────

impl Memory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            messages: Vec::with_capacity(cap),
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
        Self { messages }
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

    pub fn message_count(&self) -> usize {
        self.messages.len()
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
        assert_eq!(mem.message_count(), 1);
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
            let mut w = mem.write().unwrap();
            w.push(user_msg("hello"));
        }
        {
            let r = mem.read().unwrap();
            assert_eq!(r.message_count(), 1);
        }
    }

    #[test]
    fn test_from_vec() {
        let msgs = vec![user_msg("a"), assistant_msg("b")];
        let mem = Memory::from(msgs);
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_builder_with_messages() {
        let mem = Memory::builder()
            .with_messages(vec![user_msg("preloaded")])
            .build();
        assert_eq!(mem.message_count(), 1);
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
}
