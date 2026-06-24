//! # Memory — Conversation History with Compaction
//!
//! This module stores the agent's conversation history (a sequence of
//! [`Message`]s) and provides a **two-phase compaction** mechanism for
//! keeping context within token-budget limits.
//!
//! ## Key types
//!
//! | Type | Role |
//! |------|------|
//! | [`Memory`] | Owned conversation history with compaction logic |
//! | [`SharedMemory`] | `Arc<RwLock<Memory>>` — thread-safe wrapper for async tasks |
//! | [`MemoryBuilder`] | Fluent constructor for [`Memory`] |
//! | [`CompactSignal`] | Advisory signal returned by [`Memory::push`] |
//! | [`MemoryError`] | Errors produced by compaction operations |
//!
//! ## Two-phase compaction
//!
//! Rather than coupling [`Memory`] to any specific LLM provider, the
//! module exposes two primitive operations that the caller composes:
//!
//! 1. **[`Memory::drain_for_compact`]** — removes old non-System messages,
//!    leaving the most recent N in place. Returns the drained messages
//!    for summarisation.
//! 2. **[`Memory::apply_compact`]** — inserts a summary string as a new
//!    System message at position 0.
//!
//! A convenience free function [`compact_with_deepseek`] ties the two
//! together using a [`DeepSeekClient`] pointed at a flash model.
//!
//! ## Design decisions
//!
//! - **System messages are never drained.** They carry persistent
//!   instructions (system prompt, previous summaries) that must survive
//!   compaction so the model remembers who it is.
//! - **Character-count threshold, not token-count.** Tokenisers differ
//!   across models and are expensive to run. Character count is a fast,
//!   deterministic proxy — 2 M chars ≈ 500k–1 M tokens depending on
//!   language.
//! - **`CompactSignal` is advisory.** The caller decides *when* to
//!   compact; `push` merely signals that the threshold has been crossed.

use std::fmt;
use std::sync::{Arc, RwLock};

use crate::core::client::{DeepSeekClient, DeepSeekRequest, Message, Role};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default character-count threshold that triggers a [`CompactSignal::NeedsCompact`].
///
/// Roughly 2 M characters approximates 500k–1 M tokens depending on
/// language mix (English ≈ 4 chars/token, Chinese ≈ 1.5–2 chars/token).
/// This is a conservative safety net; true model context windows are
/// larger but we want headroom for the response.
pub const DEFAULT_COMPACT_CHARS: usize = 2_000_000;

/// Default number of most-recent non-System messages preserved verbatim
/// during compaction. Older non-System messages are fed to the summariser.
pub const DEFAULT_KEEP_LAST_N: usize = 10;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors produced by memory compaction operations.
///
/// This type exists so [`Memory`] does not couple directly to any
/// particular LLM provider's error type.
#[derive(Debug, Clone)]
pub enum MemoryError {
    /// The external summariser (e.g. an LLM) failed to produce a summary.
    SummariserFailed(String),
    /// [`Memory::drain_for_compact`] found no messages worth draining —
    /// the conversation is short enough that compaction is unnecessary.
    NothingToCompact,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SummariserFailed(reason) => {
                write!(f, "summariser failed: {reason}")
            }
            Self::NothingToCompact => {
                write!(f, "nothing to compact — conversation is within budget")
            }
        }
    }
}

impl std::error::Error for MemoryError {}

// ── CompactSignal ─────────────────────────────────────────────────────────────

/// Signal returned by [`Memory::push`].
///
/// This is purely advisory — the caller may defer compaction to a
/// convenient point (e.g. just before the next LLM call).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactSignal {
    /// Context is within the configured budget; no action needed.
    WithinBudget,
    /// Character count has exceeded [`Memory::compact_threshold`];
    /// the caller should consider calling [`Memory::drain_for_compact`]
    /// before the next LLM round-trip.
    NeedsCompact,
}

// ── Memory ────────────────────────────────────────────────────────────────────

/// In-memory conversation history with configurable compaction behaviour.
///
/// Stores a sequence of [`Message`]s and provides context-length-aware
/// compaction primitives. **Not** thread-safe by itself — wrap in
/// [`SharedMemory`] for multi-task access.
///
/// # Examples
///
/// ```
/// use agent_oxide::core::client::{Message, Role};
/// use agent_oxide::memory::Memory;
///
/// let mut mem = Memory::new();
/// mem.push(Message::new(Role::System, "You are a helpful assistant."));
/// mem.push(Message::new(Role::User, "Hello!"));
/// assert_eq!(mem.message_count(), 2);
/// ```
#[derive(Clone, Debug)]
pub struct Memory {
    messages: Vec<Message>,
    compact_threshold: usize,
    keep_last_n: usize,
}

/// Thread-safe shared conversation memory for use across tokio tasks.
///
/// # Example
///
/// ```ignore
/// let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
/// mem.write().unwrap().push(Message::new(Role::User, "Hello"));
/// let all: Vec<Message> = mem.read().unwrap().to_context_vec();
/// ```
pub type SharedMemory = Arc<RwLock<Memory>>;

// ── Builder ───────────────────────────────────────────────────────────────────

/// Fluent builder for [`Memory`].
///
/// All fields have sensible defaults, so callers only set what they need.
///
/// # Example
///
/// ```
/// use agent_oxide::memory::Memory;
///
/// let mem = Memory::builder()
///     .threshold(500_000)
///     .keep_last(15)
///     .build();
/// assert_eq!(mem.compact_threshold(), 500_000);
/// ```
#[derive(Debug, Clone)]
pub struct MemoryBuilder {
    messages: Vec<Message>,
    threshold: usize,
    keep_last: usize,
}

impl MemoryBuilder {
    /// Sets the compaction threshold in characters (default:
    /// [`DEFAULT_COMPACT_CHARS`]).
    pub fn threshold(mut self, chars: usize) -> Self {
        self.threshold = chars;
        self
    }

    /// Sets how many recent non-System messages to keep verbatim during
    /// compaction (default: [`DEFAULT_KEEP_LAST_N`]).
    pub fn keep_last(mut self, n: usize) -> Self {
        self.keep_last = n;
        self
    }

    /// Seeds the memory with an existing conversation history.
    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    /// Constructs the [`Memory`] instance.
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
    /// Creates an empty memory with default threshold and keep-last values.
    ///
    /// ```
    /// use agent_oxide::memory::Memory;
    ///
    /// let mem = Memory::new();
    /// assert!(mem.messages().is_empty());
    /// ```
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            compact_threshold: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    /// Creates an empty memory with a pre-allocated capacity for `cap`
    /// messages (reduces reallocations during growth).
    ///
    /// ```
    /// use agent_oxide::memory::Memory;
    ///
    /// let mem = Memory::with_capacity(100);
    /// assert!(mem.messages().is_empty());
    /// // internal Vec has room for ≥100 messages
    /// ```
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            messages: Vec::with_capacity(cap),
            compact_threshold: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    /// Creates an empty memory with a custom compaction threshold
    /// (in characters, not tokens).
    ///
    /// ```
    /// use agent_oxide::memory::Memory;
    ///
    /// let mem = Memory::with_threshold(42_000);
    /// assert_eq!(mem.compact_threshold(), 42_000);
    /// ```
    pub fn with_threshold(threshold: usize) -> Self {
        Self {
            messages: Vec::new(),
            compact_threshold: threshold,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    /// Returns a [`MemoryBuilder`] for fluent construction.
    ///
    /// ```
    /// use agent_oxide::memory::Memory;
    ///
    /// let mem = Memory::builder()
    ///     .threshold(100_000)
    ///     .keep_last(5)
    ///     .build();
    /// assert_eq!(mem.compact_threshold(), 100_000);
    /// ```
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
    /// Appends a message to the conversation history.
    ///
    /// Returns [`CompactSignal::NeedsCompact`] when the total character
    /// count of all stored messages exceeds the configured threshold.
    /// The signal is advisory — the caller may defer compaction to a
    /// convenient point before the next LLM round-trip.
    ///
    /// ```
    /// use agent_oxide::core::client::{Message, Role};
    /// use agent_oxide::memory::{CompactSignal, Memory};
    ///
    /// let mut mem = Memory::new();
    /// let signal = mem.push(Message::new(Role::User, "Hello"));
    /// assert_eq!(signal, CompactSignal::WithinBudget);
    /// ```
    pub fn push(&mut self, message: Message) -> CompactSignal {
        self.messages.push(message);
        if self.total_chars() > self.compact_threshold {
            CompactSignal::NeedsCompact
        } else {
            CompactSignal::WithinBudget
        }
    }

    /// Returns a reference to all stored messages.
    ///
    /// ```
    /// use agent_oxide::core::client::{Message, Role};
    /// use agent_oxide::memory::Memory;
    ///
    /// let mut mem = Memory::new();
    /// mem.push(Message::new(Role::User, "Hi"));
    /// assert_eq!(mem.messages().len(), 1);
    /// ```
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Clones the messages into an owned `Vec<Message>`, suitable for
    /// building a [`DeepSeekRequest`].
    ///
    /// ```
    /// use agent_oxide::core::client::{Message, Role};
    /// use agent_oxide::memory::Memory;
    ///
    /// let mut mem = Memory::new();
    /// mem.push(Message::new(Role::User, "Hello"));
    /// let v = mem.to_context_vec();
    /// assert_eq!(v.len(), 1);
    /// ```
    pub fn to_context_vec(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Returns the number of messages currently stored.
    ///
    /// Idiomatic alias for `self.messages().len()`.
    ///
    /// ```
    /// use agent_oxide::core::client::{Message, Role};
    /// use agent_oxide::memory::Memory;
    ///
    /// let mut mem = Memory::new();
    /// mem.push(Message::new(Role::User, "a"));
    /// mem.push(Message::new(Role::User, "b"));
    /// assert_eq!(mem.len(), 2);
    /// assert_eq!(mem.message_count(), 2);  // equivalent
    /// ```
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Returns `true` if the conversation history is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

// ── Context Length ────────────────────────────────────────────────────────────

impl Memory {
    /// Returns the total number of characters across all `content`
    /// fields in every stored message.
    ///
    /// This is a rough proxy for token count — cheap, deterministic,
    /// and model-agnostic. Use it alongside [`needs_compact`](Self::needs_compact)
    /// to decide when to drain.
    ///
    /// ```
    /// use agent_oxide::core::client::{Message, Role};
    /// use agent_oxide::memory::Memory;
    ///
    /// let mut mem = Memory::new();
    /// mem.push(Message::new(Role::User, "abc"));   // 3 chars
    /// mem.push(Message::new(Role::User, "defg"));  // 4 chars
    /// assert_eq!(mem.total_chars(), 7);
    /// ```
    pub fn total_chars(&self) -> usize {
        self.messages.iter().map(|m| m.content.len()).sum()
    }

    /// Returns the number of messages currently stored.
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Checks whether the total character count exceeds the configured
    /// threshold.
    ///
    /// ```
    /// use agent_oxide::core::client::{Message, Role};
    /// use agent_oxide::memory::Memory;
    ///
    /// let mut mem = Memory::with_threshold(5);
    /// assert!(!mem.needs_compact());
    /// mem.push(Message::new(Role::User, "hello world")); // 11 chars > 5
    /// assert!(mem.needs_compact());
    /// ```
    pub fn needs_compact(&self) -> bool {
        self.total_chars() > self.compact_threshold
    }

    /// Returns the current compaction threshold in characters.
    pub fn compact_threshold(&self) -> usize {
        self.compact_threshold
    }

    /// Sets a new compaction threshold.
    pub fn set_compact_threshold(&mut self, threshold: usize) {
        self.compact_threshold = threshold;
    }

    /// Returns how many recent non-System messages are preserved during
    /// compaction.
    pub fn keep_last_n(&self) -> usize {
        self.keep_last_n
    }
}

// ── Compaction ────────────────────────────────────────────────────────────────

impl Memory {
    /// Drains old non-System messages, leaving the most recent
    /// `self.keep_last_n` non-System messages in place.
    ///
    /// **System messages are never drained** — they carry persistent
    /// instructions (system prompt, previous summaries) that must
    /// survive compaction.
    ///
    /// Returns the drained messages in their original chronological
    /// order, ready for summarisation. Returns an empty `Vec` if there
    /// are no excess non-System messages to drain.
    ///
    /// # Algorithm
    ///
    /// 1. Count how many non-System messages exist.
    /// 2. If `count ≤ keep_last_n`, there is nothing to drain — return.
    /// 3. Otherwise, walk every message:
    ///    - The first `(count - keep_last_n)` non-System messages go to
    ///      the drained `Vec`.
    ///    - Everything else (all System messages + the last `keep_last_n`
    ///      non-System messages) stays.
    ///
    /// ```
    /// use agent_oxide::core::client::{Message, Role};
    /// use agent_oxide::memory::Memory;
    ///
    /// let mut mem = Memory::builder().keep_last(3).build();
    /// mem.push(Message::new(Role::System, "Be helpful."));
    /// mem.push(Message::new(Role::User, "q1"));
    /// mem.push(Message::new(Role::User, "q2"));
    /// mem.push(Message::new(Role::User, "q3"));
    /// mem.push(Message::new(Role::User, "q4"));
    ///
    /// let drained = mem.drain_for_compact();
    /// assert_eq!(drained.len(), 1);         // only "q1" was old enough
    /// assert_eq!(drained[0].content, "q1");
    /// assert_eq!(mem.message_count(), 4);   // 1 System + 3 kept non-System
    /// ```
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
    ///
    /// Void if `summary` is empty — callers need not check before calling.
    ///
    /// This is the second half of the two-phase compaction pattern.
    /// Call [`drain_for_compact`](Self::drain_for_compact) first, send the
    /// drained messages to a summariser (LLM), then feed the result here.
    ///
    /// ```
    /// use agent_oxide::core::client::{Message, Role};
    /// use agent_oxide::memory::Memory;
    ///
    /// let mut mem = Memory::builder().keep_last(3).build();
    /// mem.push(Message::new(Role::System, "Be helpful."));
    /// for i in 0..5 {
    ///     mem.push(Message::new(Role::User, format!("q{i}")));
    /// }
    ///
    /// let drained = mem.drain_for_compact();
    /// assert_eq!(drained.len(), 2);  // q0, q1
    ///
    /// mem.apply_compact("Earlier: user asked q0 and q1.".into());
    /// assert_eq!(mem.messages()[0].role, Role::System);
    /// assert_eq!(mem.messages()[0].content, "Earlier: user asked q0 and q1.");
    /// ```
    pub fn apply_compact(&mut self, summary: String) {
        if summary.is_empty() {
            return;
        }
        self.messages.insert(0, Message::new(Role::System, summary));
    }
}

// ── Convenience: DeepSeek-backed compaction ───────────────────────────────────

/// Compacts `memory` using the supplied [`DeepSeekClient`] for
/// summarisation.
///
/// This is a convenience function that composes the two-phase API:
///
/// 1. [`Memory::drain_for_compact`] — drains old non-System messages
/// 2. Sends them to the flash model at `model` for summarisation
/// 3. [`Memory::apply_compact`] — inserts the summary at position 0
///
/// Returns `Ok(())` on success, [`MemoryError::NothingToCompact`] if
/// there were no messages to drain, or [`MemoryError::SummariserFailed`]
/// if the LLM call failed.
///
/// # Panics
///
/// Panics if `DEEPSEEK_API` is not set — the client handles this.
pub async fn compact_with_deepseek(
    memory: &mut Memory,
    client: &DeepSeekClient,
    model: &str,
) -> Result<(), MemoryError> {
    let old = memory.drain_for_compact();
    if old.is_empty() {
        return Err(MemoryError::NothingToCompact);
    }

    // Build a compact transcript for the summariser.
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

    let request = DeepSeekRequest::new(model, vec![Message::new(Role::User, prompt)]);

    let response = client
        .send(request)
        .await
        .map_err(|e| MemoryError::SummariserFailed(format!("LLM call failed: {e}")))?;

    let summary = response
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();

    memory.apply_compact(summary);
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Human-readable label for each [`Role`], used when formatting
/// conversation transcripts for summarisation.
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

    // ── Helpers ──────────────────────────────────────────────────────────

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

    // ── Construction ─────────────────────────────────────────────────────

    #[test]
    fn test_new_creates_empty() {
        let mem = Memory::new();
        assert!(mem.messages().is_empty());
        assert_eq!(mem.message_count(), 0);
        assert_eq!(mem.compact_threshold(), DEFAULT_COMPACT_CHARS);
        assert_eq!(mem.keep_last_n(), DEFAULT_KEEP_LAST_N);
    }

    #[test]
    fn test_default_equals_new() {
        let m1 = Memory::new();
        let m2 = Memory::default();
        assert_eq!(m1.message_count(), m2.message_count());
        assert_eq!(m1.compact_threshold(), m2.compact_threshold());
        assert_eq!(m1.keep_last_n(), m2.keep_last_n());
    }

    #[test]
    fn test_with_capacity_pre_allocates() {
        let mem = Memory::with_capacity(100);
        assert!(mem.messages().is_empty());
        // Verify the internal Vec reserved room — field is private, but
        // we can check via the standard Vec invariant: capacity ≥ requested.
        assert!(mem.messages.capacity() >= 100);
    }

    #[test]
    fn test_with_threshold_custom() {
        let mem = Memory::with_threshold(500);
        assert_eq!(mem.compact_threshold(), 500);
        assert!(mem.messages().is_empty());
    }

    #[test]
    fn test_from_vec() {
        let msgs = vec![user_msg("a"), assistant_msg("b")];
        let mem = Memory::from(msgs);
        assert_eq!(mem.messages().len(), 2);
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_clone_independent() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        let mut cloned = mem.clone();
        cloned.push(assistant_msg("world"));
        // Original unchanged
        assert_eq!(mem.message_count(), 1);
        assert_eq!(cloned.message_count(), 2);
    }

    #[test]
    fn test_is_empty() {
        let mut mem = Memory::new();
        assert!(mem.is_empty());
        mem.push(user_msg("hi"));
        assert!(!mem.is_empty());
    }

    // ── Builder ──────────────────────────────────────────────────────────

    #[test]
    fn test_builder_defaults() {
        let mem = Memory::builder().build();
        assert!(mem.messages().is_empty());
        assert_eq!(mem.compact_threshold(), DEFAULT_COMPACT_CHARS);
        assert_eq!(mem.keep_last_n(), DEFAULT_KEEP_LAST_N);
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
    fn test_builder_builds_equivalent_to_new() {
        let m1 = Memory::new();
        let m2 = Memory::builder().build();
        assert_eq!(m1.compact_threshold(), m2.compact_threshold());
        assert_eq!(m1.keep_last_n(), m2.keep_last_n());
    }

    // ── Push ─────────────────────────────────────────────────────────────

    #[test]
    fn test_push_appends_message() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        assert_eq!(mem.message_count(), 1);
        assert_eq!(mem.messages()[0].content, "hello");
        assert_eq!(mem.messages()[0].role, Role::User);
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
        let signal = mem.push(user_msg("hello world")); // 11 chars > 5
        assert_eq!(signal, CompactSignal::NeedsCompact);
    }

    #[test]
    fn test_push_returns_within_budget_at_exact_threshold() {
        let mut mem = Memory::with_threshold(5);
        // "hello" is exactly 5 chars — NOT over threshold
        let signal = mem.push(user_msg("hello"));
        assert_eq!(signal, CompactSignal::WithinBudget);
    }

    // ── Messages / to_context_vec ────────────────────────────────────────

    #[test]
    fn test_messages_returns_all() {
        let mut mem = Memory::new();
        mem.push(user_msg("q1"));
        mem.push(assistant_msg("a1"));
        mem.push(user_msg("q2"));
        assert_eq!(mem.messages().len(), 3);
    }

    #[test]
    fn test_to_context_vec_returns_owned() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        let v = mem.to_context_vec();
        assert_eq!(v.len(), 1);
        // Modifying the returned vec doesn't affect memory
        drop(v);
        assert_eq!(mem.message_count(), 1);
    }

    #[test]
    fn test_messages_returns_reference_not_copy() {
        let mut mem = Memory::new();
        mem.push(user_msg("test"));
        let r = mem.messages();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].content, "test");
    }

    // ── Len ──────────────────────────────────────────────────────────────

    #[test]
    fn test_len_and_message_count_agree() {
        let mut mem = Memory::new();
        mem.push(user_msg("a"));
        mem.push(user_msg("b"));
        assert_eq!(mem.len(), 2);
        assert_eq!(mem.message_count(), mem.len());
    }

    // ── Context Length ───────────────────────────────────────────────────

    #[test]
    fn test_total_chars_empty() {
        let mem = Memory::new();
        assert_eq!(mem.total_chars(), 0);
    }

    #[test]
    fn test_total_chars_sums_content() {
        let mut mem = Memory::new();
        mem.push(user_msg("abc")); // 3
        mem.push(assistant_msg("defg")); // 4
        assert_eq!(mem.total_chars(), 7);
    }

    #[test]
    fn test_total_chars_with_tool_messages() {
        let mut mem = Memory::new();
        let mut tc_msg = make_msg(Role::Assistant, "calling tool");
        tc_msg.tool_call_id = Some("call_1".into());
        mem.push(tc_msg);
        // Only "calling tool" (12 chars) counts — tool_call_id is ignored
        assert_eq!(mem.total_chars(), 12);
    }

    #[test]
    fn test_message_count() {
        let mut mem = Memory::new();
        assert_eq!(mem.message_count(), 0);
        mem.push(user_msg("a"));
        mem.push(user_msg("b"));
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_needs_compact_false_when_empty() {
        let mem = Memory::new();
        assert!(!mem.needs_compact());
    }

    #[test]
    fn test_needs_compact_false_under_threshold() {
        let mut mem = Memory::with_threshold(100);
        mem.push(user_msg("short"));
        assert!(!mem.needs_compact());
    }

    #[test]
    fn test_needs_compact_true_when_over_threshold() {
        let mut mem = Memory::with_threshold(2);
        mem.push(user_msg("abc")); // 3 chars > 2
        assert!(mem.needs_compact());
    }

    #[test]
    fn test_set_compact_threshold() {
        let mut mem = Memory::new();
        assert_eq!(mem.compact_threshold(), DEFAULT_COMPACT_CHARS);
        mem.set_compact_threshold(42);
        assert_eq!(mem.compact_threshold(), 42);
    }

    #[test]
    fn test_keep_last_n_default() {
        let mem = Memory::new();
        assert_eq!(mem.keep_last_n(), DEFAULT_KEEP_LAST_N);
    }

    // ── Compaction: drain_for_compact ────────────────────────────────────

    #[test]
    fn test_drain_preserves_last_n_messages() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 5); // 15 - 10 = 5 drained
        assert_eq!(mem.message_count(), 10);
        // First remaining message should be msg_5 (0-indexed)
        assert_eq!(mem.messages()[0].content, "msg_5");
        // Last remaining should be msg_14
        assert_eq!(mem.messages()[9].content, "msg_14");
    }

    #[test]
    fn test_drain_returns_old_in_order() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 5);
        for (i, m) in old.iter().enumerate() {
            assert_eq!(m.content, format!("msg_{i}"));
        }
    }

    #[test]
    fn test_drain_noop_when_fewer_than_keep() {
        let mut mem = Memory::new();
        mem.push(user_msg("a"));
        mem.push(user_msg("b"));
        mem.push(user_msg("c"));
        let old = mem.drain_for_compact();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 3); // unchanged
    }

    #[test]
    fn test_drain_noop_when_exactly_keep() {
        let mut mem = Memory::new();
        for i in 0..DEFAULT_KEEP_LAST_N {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), DEFAULT_KEEP_LAST_N);
    }

    #[test]
    fn test_drain_one_extra() {
        let mut mem = Memory::new();
        for i in 0..=DEFAULT_KEEP_LAST_N {
            // 11 messages total
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].content, "msg_0");
        assert_eq!(mem.message_count(), DEFAULT_KEEP_LAST_N); // 10 kept
    }

    #[test]
    fn test_drain_empty_memory() {
        let mut mem = Memory::new();
        let old = mem.drain_for_compact();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 0);
    }

    #[test]
    fn test_drain_respects_custom_keep_last() {
        let mut mem = Memory::builder().keep_last(3).build();
        for i in 0..10 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 7); // 10 - 3 = 7
        assert_eq!(mem.message_count(), 3);
        assert_eq!(mem.messages()[0].content, "msg_7");
        assert_eq!(mem.messages()[2].content, "msg_9");
    }

    // ── Compaction: apply_compact ────────────────────────────────────────

    #[test]
    fn test_apply_compact_inserts_at_front() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.apply_compact("Summary of earlier discussion.".into());
        assert_eq!(mem.message_count(), 2);
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "Summary of earlier discussion.");
        assert_eq!(mem.messages()[1].content, "hello"); // original still there
    }

    #[test]
    fn test_apply_compact_void_on_empty_summary() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.apply_compact(String::new());
        mem.apply_compact("".into());
        assert_eq!(mem.message_count(), 1);
    }

    // ── Compaction: full-cycle drain + apply ─────────────────────────────

    #[test]
    fn test_drain_and_apply_full_cycle() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 5);
        let summary = format!("{} messages summarized", old.len());
        mem.apply_compact(summary);
        assert_eq!(mem.message_count(), 11); // 1 summary + 10 kept
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "5 messages summarized");
    }

    #[test]
    fn test_drain_and_apply_noop_when_few_messages() {
        let mut mem = Memory::new();
        mem.push(user_msg("only one"));
        let old = mem.drain_for_compact();
        assert!(old.is_empty());
        // apply_compact is a no-op since there's nothing to apply
        assert_eq!(mem.message_count(), 1);
    }

    // ── SharedMemory ─────────────────────────────────────────────────────

    #[test]
    fn test_shared_memory_write_read() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        {
            let mut w = mem.write().unwrap();
            w.push(user_msg("hello"));
            w.push(assistant_msg("world"));
        }
        {
            let r = mem.read().unwrap();
            assert_eq!(r.message_count(), 2);
            assert_eq!(r.messages()[0].content, "hello");
            assert_eq!(r.messages()[1].content, "world");
        }
    }

    #[test]
    fn test_shared_memory_concurrent_reads() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        {
            let mut w = mem.write().unwrap();
            w.push(user_msg("data"));
        }
        // Two read locks can coexist
        let r1 = mem.read().unwrap();
        let r2 = mem.read().unwrap();
        assert_eq!(r1.message_count(), 1);
        assert_eq!(r2.message_count(), 1);
        drop(r1);
        drop(r2);
    }

    #[test]
    fn test_shared_memory_clone_increases_ref_count() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        let mem2 = Arc::clone(&mem);
        {
            let mut w = mem2.write().unwrap();
            w.push(user_msg("via clone"));
        }
        let r = mem.read().unwrap();
        assert_eq!(r.message_count(), 1);
    }

    // ── Edge Cases ───────────────────────────────────────────────────────

    #[test]
    fn test_large_single_message_triggers_needs_compact() {
        let mut mem = Memory::with_threshold(10);
        let signal = mem.push(user_msg("this is a long message"));
        assert_eq!(signal, CompactSignal::NeedsCompact);
    }

    #[test]
    fn test_multiple_pushes_accumulate() {
        let mut mem = Memory::with_threshold(10);
        assert_eq!(mem.push(user_msg("abc")), CompactSignal::WithinBudget); // 3
        assert_eq!(mem.push(user_msg("def")), CompactSignal::WithinBudget); // 6
        assert_eq!(mem.push(user_msg("ghij")), CompactSignal::WithinBudget); // 10
        // Next push crosses threshold
        assert_eq!(mem.push(user_msg("k")), CompactSignal::NeedsCompact); // 11
    }

    #[test]
    fn test_system_message_content_counts() {
        let mut mem = Memory::with_threshold(10);
        mem.push(sys_msg("You are a helpful assistant"));
        // "You are a helpful assistant" = 28 chars > 10
        assert!(mem.needs_compact());
    }

    #[test]
    fn test_drain_preserves_tool_messages_in_keep_window() {
        let mut mem = Memory::new();
        // Build: user → assistant(tool_calls) → tool → assistant → ... (12 messages)
        for i in 0..4 {
            mem.push(user_msg(&format!("question_{i}")));
            let mut tc_msg = assistant_msg(&format!("calling tool_{i}"));
            tc_msg.tool_call_id = Some(format!("call_{i}"));
            mem.push(tc_msg);
            mem.push(make_msg(Role::Tool, &format!("tool_result_{i}")));
        }
        // 12 messages total, DEFAULT_KEEP_LAST_N = 10
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 2); // first 2 drained
        assert_eq!(mem.message_count(), 10);
    }

    // ── Compaction: System message preservation ──────────────────────────

    #[test]
    fn test_drain_preserves_system_messages_at_front() {
        let mut mem = Memory::new();
        // System message at the very beginning
        mem.push(sys_msg("You are a helpful assistant"));
        // Then 12 user messages (more than DEFAULT_KEEP_LAST_N)
        for i in 0..12 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        assert_eq!(mem.message_count(), 13); // 1 system + 12 user

        let old = mem.drain_for_compact();
        // 12 non-system - 10 kept = 2 drained
        assert_eq!(old.len(), 2);
        assert_eq!(old[0].content, "msg_0");
        assert_eq!(old[1].content, "msg_1");
        // System message preserved + 10 recent non-system = 11 total
        assert_eq!(mem.message_count(), 11);
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "You are a helpful assistant");
        // Recent user messages follow
        assert_eq!(mem.messages()[1].content, "msg_2");
        assert_eq!(mem.messages()[10].content, "msg_11");
    }

    #[test]
    fn test_drain_preserves_system_messages_interleaved() {
        let mut mem = Memory::new();
        mem.push(sys_msg("System prompt 1"));
        mem.push(user_msg("msg_0"));
        mem.push(assistant_msg("msg_1"));
        mem.push(sys_msg("System prompt 2"));
        mem.push(user_msg("msg_2"));
        mem.push(assistant_msg("msg_3"));
        mem.push(user_msg("msg_4"));
        mem.push(assistant_msg("msg_5"));
        mem.push(user_msg("msg_6"));
        mem.push(assistant_msg("msg_7"));
        mem.push(user_msg("msg_8"));
        mem.push(assistant_msg("msg_9"));
        mem.push(user_msg("msg_10"));
        mem.push(assistant_msg("msg_11"));
        // 14 total: 2 system + 12 non-system

        let old = mem.drain_for_compact();
        // 12 non-system - 10 kept = 2 drained (msg_0, msg_1)
        assert_eq!(old.len(), 2);
        assert!(!old.iter().any(|m| m.role == Role::System));
        // 2 system + 10 non-system = 12 remaining
        assert_eq!(mem.message_count(), 12);
        // Both system messages are preserved
        let system_msgs: Vec<_> = mem
            .messages()
            .iter()
            .filter(|m| m.role == Role::System)
            .collect();
        assert_eq!(system_msgs.len(), 2);
        assert_eq!(system_msgs[0].content, "System prompt 1");
        assert_eq!(system_msgs[1].content, "System prompt 2");
    }

    #[test]
    fn test_drain_noop_when_only_system_messages() {
        let mut mem = Memory::new();
        mem.push(sys_msg("System A"));
        mem.push(sys_msg("System B"));
        mem.push(sys_msg("System C"));

        let old = mem.drain_for_compact();
        // 0 non-system → nothing to drain
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 3); // all system messages kept
    }

    #[test]
    fn test_drain_drains_only_non_system() {
        let mut mem = Memory::new();
        mem.push(sys_msg("Important instructions"));
        mem.push(user_msg("q1"));
        mem.push(assistant_msg("a1"));
        mem.push(user_msg("q2"));
        mem.push(assistant_msg("a2"));
        mem.push(user_msg("q3"));
        mem.push(assistant_msg("a3"));
        mem.push(user_msg("q4"));
        mem.push(assistant_msg("a4"));
        mem.push(user_msg("q5"));
        mem.push(assistant_msg("a5"));
        mem.push(user_msg("q6"));
        // 12 total: 1 system + 11 non-system

        let old = mem.drain_for_compact();
        // 11 non-system - 10 kept = 1 drained (q1)
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].role, Role::User);
        assert_eq!(old[0].content, "q1");
        // System message + 10 non-system = 11 remaining
        assert_eq!(mem.message_count(), 11);
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[1].content, "a1"); // first kept non-system
    }

    #[test]
    fn test_drain_and_apply_preserves_system_messages_full_cycle() {
        let mut mem = Memory::new();
        mem.push(sys_msg("System instructions"));
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        // 16 total: 1 system + 15 user

        let old = mem.drain_for_compact();
        // Only non-system messages are presented for summarization
        assert!(!old.iter().any(|m| m.role == Role::System));
        assert_eq!(old.len(), 5); // 15 - 10 = 5
        let summary = format!("{} messages summarized", old.len());
        mem.apply_compact(summary);

        // 1 summary + 1 original system + 10 recent non-system = 12
        assert_eq!(mem.message_count(), 12);
        // First is the summary System message
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "5 messages summarized");
        // Second is the original System message (preserved)
        assert_eq!(mem.messages()[1].role, Role::System);
        assert_eq!(mem.messages()[1].content, "System instructions");
        // Then the 10 recent non-system messages
        assert_eq!(mem.messages()[2].content, "msg_5");
        assert_eq!(mem.messages()[11].content, "msg_14");
    }

    // ── MemoryError ──────────────────────────────────────────────────────

    #[test]
    fn test_memory_error_display() {
        let e = MemoryError::NothingToCompact;
        assert!(e.to_string().contains("nothing to compact"));

        let e = MemoryError::SummariserFailed("timeout".into());
        assert!(e.to_string().contains("summariser failed"));
        assert!(e.to_string().contains("timeout"));
    }

    // ── role_label ───────────────────────────────────────────────────────

    #[test]
    fn test_role_label_all_variants() {
        assert_eq!(role_label(Role::System), "System");
        assert_eq!(role_label(Role::User), "User");
        assert_eq!(role_label(Role::Assistant), "Assistant");
        assert_eq!(role_label(Role::Tool), "Tool");
    }
}
