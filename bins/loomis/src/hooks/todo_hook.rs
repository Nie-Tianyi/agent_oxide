//! Hook that maintains a `[TODO]` System message in memory.
//!
//! Fires in [`on_llm_start`](engine::AgentHook::on_llm_start), which runs
//! **after** all tool results have been written to memory.  This avoids the
//! API message-ordering constraint: an assistant message with `tool_calls`
//! must be immediately followed by tool-result messages — a System message
//! injected during tool execution would break that ordering.
//!
//! The hook reads the shared [`TodoItem`](crate::tools::TodoItem) list and
//! ensures exactly one `[TODO]` System message exists in memory, updating
//! it in-place when the list changes so the LLM always sees its current plan.

use std::sync::{Arc, RwLock};

use engine::AgentHook;
use memory::SharedMemory;
use provider::{Message, Role};

use crate::tools::{TODO_MARKER, TodoItem};

// ── TodoListHook ─────────────────────────────────────────────────────────────

/// Maintains the `[TODO]` System message in conversation memory.
///
/// Reads the shared todo-list state on every LLM call and synchronises
/// it to memory.  The hook is stateless — it always derives the System
/// message content from the current state, so it's safe across `/new`,
/// thread resume, and compaction.
pub struct TodoListHook {
    /// Shared todo list — read every `on_llm_start`.
    state: Arc<RwLock<Vec<TodoItem>>>,
}

impl TodoListHook {
    pub fn new(state: Arc<RwLock<Vec<TodoItem>>>) -> Self {
        Self { state }
    }
}

impl AgentHook for TodoListHook {
    /// Synchronise the `[TODO]` System message with the current shared state.
    ///
    /// Called by the agent loop **before** building the context vector for
    /// the next LLM call — all tool results from the previous step are
    /// already committed to memory, so inserting a System message here does
    /// not violate any API ordering constraint.
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let state = match self.state.read() {
            Ok(s) => s,
            Err(_) => return, // lock poisoned — skip this cycle
        };

        let content = if state.is_empty() {
            // Remove the [TODO] message entirely when the list is empty.
            None
        } else {
            let json = match serde_json::to_string(&*state) {
                Ok(j) => j,
                Err(_) => return,
            };
            Some(format!("{TODO_MARKER} {json}"))
        };

        let mut mem = match memory.write() {
            Ok(m) => m,
            Err(_) => return,
        };

        // Find and remove any existing [TODO] System message(s), then
        // insert a single one at index 0 if the list is non-empty.
        mem.messages
            .retain(|m| !(m.role == Role::System && m.content.starts_with(TODO_MARKER)));

        if let Some(c) = content {
            mem.messages.insert(0, Message::new(Role::System, c));
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use memory::Memory;

    fn make_state(items: Vec<TodoItem>) -> Arc<RwLock<Vec<TodoItem>>> {
        Arc::new(RwLock::new(items))
    }

    fn make_memory() -> SharedMemory {
        Arc::new(RwLock::new(Memory::new()))
    }

    fn make_item(seq: u32, status: &str) -> TodoItem {
        TodoItem {
            content: format!("Task {seq}"),
            status: status.to_string(),
            active_form: format!("Doing task {seq}"),
        }
    }

    #[test]
    fn test_inserts_todo_system_message_when_list_non_empty() {
        let state = make_state(vec![make_item(1, "pending"), make_item(2, "in_progress")]);
        let memory = make_memory();
        let hook = TodoListHook::new(state);

        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let todo_msg = mem
            .messages
            .iter()
            .find(|m| m.role == Role::System && m.content.starts_with(TODO_MARKER));
        assert!(todo_msg.is_some(), "expected [TODO] System message");
        let content = &todo_msg.unwrap().content;
        assert!(content.contains("Task 1"));
        assert!(content.contains("Task 2"));
    }

    #[test]
    fn test_removes_todo_system_message_when_list_empty() {
        let state = make_state(vec![]);
        let memory = make_memory();

        // Pre-seed memory with a [TODO] message
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(
                Role::System,
                format!("{TODO_MARKER} [{{\"content\":\"old\"}}]"),
            ));
        }

        let hook = TodoListHook::new(state);
        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let todo_count = mem
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.content.starts_with(TODO_MARKER))
            .count();
        assert_eq!(
            todo_count, 0,
            "expected [TODO] message to be removed when list is empty"
        );
    }

    #[test]
    fn test_replaces_existing_todo_message() {
        let state = make_state(vec![make_item(1, "completed")]);
        let memory = make_memory();

        // Pre-seed with an old [TODO] message
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(
                Role::System,
                format!("{TODO_MARKER} [{{\"content\":\"old task\"}}]"),
            ));
        }

        let hook = TodoListHook::new(state);
        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let todo_count = mem
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.content.starts_with(TODO_MARKER))
            .count();
        assert_eq!(
            todo_count, 1,
            "should still have exactly one [TODO] message"
        );

        let todo_msg = mem
            .messages
            .iter()
            .find(|m| m.role == Role::System && m.content.starts_with(TODO_MARKER))
            .unwrap();
        assert!(
            todo_msg.content.contains("Task 1"),
            "should contain new content, got: {}",
            todo_msg.content
        );
        assert!(
            !todo_msg.content.contains("old task"),
            "should NOT contain old content, got: {}",
            todo_msg.content
        );
    }

    #[test]
    fn test_does_not_affect_other_system_messages() {
        let state = make_state(vec![make_item(1, "pending")]);
        let memory = make_memory();

        // Pre-seed with a non-todo System message
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(Role::System, "Some other system message"));
        }

        let hook = TodoListHook::new(state);
        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        assert!(
            mem.messages
                .iter()
                .any(|m| m.role == Role::System && m.content == "Some other system message"),
            "non-todo System message should be preserved"
        );
    }
}
