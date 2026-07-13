//! [`TodoTool`] — lets the LLM create and manage a structured task list.
//!
//! # How it works
//!
//! The tool accepts a full list of `TodoItem`s each call (replacement semantics).
//! It writes the list into a shared `Arc<RwLock<Vec<TodoItem>>>` — read by the
//! TUI every frame for the status bar.
//!
//! A companion [`TodoListHook`](crate::hooks::TodoListHook) maintains a
//! `Role::System` message in [`SharedMemory`](memory::SharedMemory) so the
//! LLM sees its plan in context and the list survives compaction.  The hook
//! fires in [`on_llm_start`](engine::AgentHook::on_llm_start), AFTER all
//! tool results have been written — avoiding the API ordering constraint
//! that tool results must immediately follow assistant `tool_calls`.

use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use tools::{ProgressStream, ToolError, tool};

/// Marker prefix that identifies the todo-list System message in memory.
pub const TODO_MARKER: &str = "[TODO]";

// ── Types ────────────────────────────────────────────────────────────────────

/// A single todo item. Shared between the tool and TUI via `Arc<RwLock<…>>`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TodoItem {
    /// Brief description of the task.
    #[schemars(description = "Brief description of the task to complete.")]
    pub content: String,

    /// Current status: "pending", "in_progress", or "completed".
    #[schemars(
        description = "Current status. One of: \"pending\", \"in_progress\", \"completed\"."
    )]
    pub status: String,

    /// Present-tense label shown while the task is in progress (e.g. "Implementing TodoTool").
    #[schemars(description = "Present-tense label shown while this task is in progress.")]
    pub active_form: String,
}

// ── Args ─────────────────────────────────────────────────────────────────────

/// Arguments for the todo tool — the full list replaces any previous list.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoToolArgs {
    /// The complete list of todo items. Each call REPLACES the entire list.
    /// Include ALL items (pending, in_progress, and completed).
    #[schemars(
        description = "Full list of todos. Each call replaces the existing list completely. Include ALL items."
    )]
    pub todos: Vec<TodoItem>,
}

// ── Valid status values ──────────────────────────────────────────────────────

const VALID_STATUSES: &[&str] = &["pending", "in_progress", "completed"];

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Lets the LLM create and update a structured task list (plan) for the
/// current work session.
///
/// Use this BEFORE writing any code to plan your approach. Break the work
/// into clear steps, set each to "pending", then update them as you make
/// progress.
///
/// # Parameters
///
/// ```json
/// {
///   "todos": [
///     {"content": "Explore the codebase", "status": "completed", "active_form": "Exploring"},
///     {"content": "Implement the change", "status": "in_progress", "active_form": "Implementing"},
///     {"content": "Write tests", "status": "pending", "active_form": "Writing tests"}
///   ]
/// }
/// ```
///
/// # Response
///
/// Returns a human-readable summary like "Updated plan: 3 todos, 1 ✓, 1 in
/// progress, 1 pending".
///
/// # Persistence
///
/// The list survives compaction (stored as a System message) and persists
/// across conversation resets via `/new`.
#[tool(
    name = "todo",
    description = "Create and update a structured task list (plan) for the current work session. \
         Use this tool BEFORE writing any code to plan your approach. \
         Break the work into clear, verifiable steps.\n\n\
         Each call REPLACES the entire list — include ALL items (pending, \
         in_progress, and completed). Status values:\n\
         - \"pending\": not yet started\n\
         - \"in_progress\": currently working on (at most ONE at a time)\n\
         - \"completed\": finished\n\n\
         When to use:\n\
         - Before starting any multi-step task\n\
         - To track progress across multiple tool calls\n\
         - To mark tasks done as you complete them\n\n\
         When NOT to use:\n\
         - For trivial single-step requests\n\
         - As a substitute for actually doing the work",
    args = TodoToolArgs
)]
pub struct TodoTool {
    /// Shared todo list — written on each tool call, read by the TUI.
    state: Arc<RwLock<Vec<TodoItem>>>,
}

impl TodoTool {
    /// Creates a new TodoTool.
    ///
    /// `state` must be the same `Arc` that is passed to the TUI's `App`.
    pub fn new(state: Arc<RwLock<Vec<TodoItem>>>) -> Self {
        Self { state }
    }

    /// Core logic — validates, updates shared state, returns a summary.
    ///
    /// The companion [`TodoListHook`](crate::hooks::TodoListHook) handles
    /// injecting the list as a System message in [`on_llm_start`], which
    /// runs after all tool results have been written to memory — avoiding
    /// the API message-ordering constraint.
    fn execute_stream(&self, args: TodoToolArgs) -> Result<ProgressStream, ToolError> {
        // ── Validation ──────────────────────────────────────────────
        let mut in_progress_count = 0usize;
        for item in &args.todos {
            if !VALID_STATUSES.contains(&item.status.as_str()) {
                return Err(ToolError::InvalidArgs(format!(
                    "invalid status {:?} for todo {:?} — must be one of: {}",
                    item.status,
                    item.content,
                    VALID_STATUSES.join(", "),
                )));
            }
            if item.status == "in_progress" {
                in_progress_count += 1;
            }
        }
        if in_progress_count > 1 {
            return Err(ToolError::InvalidArgs(format!(
                "at most one todo may be \"in_progress\" at a time, found {in_progress_count}"
            )));
        }

        // ── Update shared state (for TUI and TodoListHook) ──────────
        {
            let mut state = self
                .state
                .write()
                .map_err(|e| ToolError::Execution(format!("todo state lock poisoned: {e}")))?;
            *state = args.todos.clone();
        }

        // ── Build human-readable summary ────────────────────────────
        let total = args.todos.len();
        if total == 0 {
            return Ok(ProgressStream::done("Cleared todo list (no tasks)".into()));
        }

        let pending = args.todos.iter().filter(|t| t.status == "pending").count();
        let in_progress = args
            .todos
            .iter()
            .filter(|t| t.status == "in_progress")
            .count();
        let completed = args
            .todos
            .iter()
            .filter(|t| t.status == "completed")
            .count();

        let mut parts = vec![format!("Updated plan: {total} todos")];
        if completed > 0 {
            parts.push(format!("{completed} ✓ completed"));
        }
        if in_progress > 0 {
            parts.push(format!("{in_progress} in progress"));
        }
        if pending > 0 {
            parts.push(format!("{pending} pending"));
        }

        Ok(ProgressStream::done(parts.join(", ")))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn make_state() -> Arc<RwLock<Vec<TodoItem>>> {
        Arc::new(RwLock::new(Vec::new()))
    }

    fn make_tool() -> TodoTool {
        TodoTool::new(make_state())
    }

    // ── Trait methods ──────────────────────────────────────────────

    #[test]
    fn test_name() {
        assert_eq!(make_tool().name(), "todo");
    }

    #[test]
    fn test_description() {
        let tool = make_tool();
        let desc = tool.description();
        assert!(desc.contains("task list"), "got: {desc}");
        assert!(desc.contains("plan"), "got: {desc}");
    }

    #[test]
    fn test_parameter_schema() {
        let params = make_tool().parameter_schema();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["todos"]["type"] == "array");
        assert_eq!(params["additionalProperties"], false);
    }

    // ── execute_stream ─────────────────────────────────────────────

    #[test]
    fn test_invalid_json() {
        let err = Tool::execute_stream(&make_tool(), "garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_missing_todos_field() {
        let err = Tool::execute_stream(&make_tool(), r#"{"wrong": "field"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_extra_field_rejected() {
        let err =
            Tool::execute_stream(&make_tool(), r#"{"todos": [], "extra": true}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_empty_list() {
        let result = Tool::execute_stream(&make_tool(), r#"{"todos": []}"#)
            .unwrap()
            .poll_done();
        assert!(result.contains("Cleared"));
    }

    #[test]
    fn test_valid_todos() {
        let tool = make_tool();
        let input = r#"{"todos": [
            {"content": "task 1", "status": "completed", "active_form": "Doing task 1"},
            {"content": "task 2", "status": "in_progress", "active_form": "Doing task 2"},
            {"content": "task 3", "status": "pending", "active_form": "Doing task 3"}
        ]}"#;

        let result = Tool::execute_stream(&tool, input).unwrap().poll_done();

        assert!(result.contains("3 todos"));
        assert!(result.contains("completed"));
        assert!(result.contains("in progress"));
        assert!(result.contains("pending"));

        // Verify shared state was updated
        let state = tool.state.read().unwrap();
        assert_eq!(state.len(), 3);
        assert_eq!(state[0].status, "completed");
        assert_eq!(state[1].status, "in_progress");
        assert_eq!(state[2].status, "pending");
    }

    #[test]
    fn test_invalid_status_rejected() {
        let input = r#"{"todos": [
            {"content": "bad", "status": "unknown", "active_form": "???"}
        ]}"#;
        let err = Tool::execute_stream(&make_tool(), input).unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(ref msg) if msg.contains("invalid status")),
            "expected InvalidArgs with 'invalid status', got: {err:?}"
        );
    }

    #[test]
    fn test_multiple_in_progress_rejected() {
        let input = r#"{"todos": [
            {"content": "a", "status": "in_progress", "active_form": "A"},
            {"content": "b", "status": "in_progress", "active_form": "B"}
        ]}"#;
        let err = Tool::execute_stream(&make_tool(), input).unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(ref msg) if msg.contains("at most one")),
            "expected InvalidArgs with 'at most one', got: {err:?}"
        );
    }

    #[test]
    fn test_state_cleared_on_empty_list() {
        let tool = make_tool();

        // Seed with some todos
        Tool::execute_stream(
            &tool,
            r#"{"todos": [{"content": "task", "status": "pending", "active_form": "T"}]}"#,
        )
        .unwrap();

        assert_eq!(tool.state.read().unwrap().len(), 1);

        // Clear
        Tool::execute_stream(&tool, r#"{"todos": []}"#).unwrap();

        assert!(tool.state.read().unwrap().is_empty());
    }
}
