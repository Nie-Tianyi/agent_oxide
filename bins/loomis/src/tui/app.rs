//! # App State
//!
//! The mutable state machine for the TUI: chat messages, input buffer,
//! scrolling, streaming status, and keyboard processing.
//!
//! ## File layout
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`super::messages`] | `ChatMessage`, `TuiCommand`, `ThreadPicker` types |
//! | [`super::input`] | `handle_key()`, slash commands, shell confirmation |
//! | `app` (here) | `App` struct + event application + tests |

use std::path::PathBuf;

use engine::AgentEvent;
use memory::SharedMemory;

use super::messages::{ChatMessage, ToolCallState};

// ── App ──────────────────────────────────────────────────────────────────────────

/// Mutable state owned by the TUI event loop.
///
/// All fields are updated synchronously during the render/keyboard/event cycle.
/// The `memory` field is a shared `Arc<RwLock<Memory>>` clone — read-only from
/// the TUI side (for `/stats`), written exclusively by the agent thread.
pub struct App {
    // ── Conversation ──
    pub messages: Vec<ChatMessage>,
    /// Cached line count per message, rebuilt each frame.
    /// Parallel to `messages`.
    pub line_counts: Vec<usize>,

    // ── Input ──
    pub input: String,
    /// Byte offset into `input`.
    pub input_cursor: usize,

    // ── Scrolling ──
    /// How many lines the user has scrolled up (0 = bottom).
    pub scroll_offset: usize,
    /// When `true`, new messages reset scroll to bottom.
    pub auto_scroll: bool,

    // ── Agent status ──
    pub streaming: bool,

    // ── Shared state ──
    pub model: String,
    pub memory: SharedMemory,
    pub tool_names: Vec<String>,
    /// Workspace root directory for `!` shell commands.
    pub workspace_root: PathBuf,

    // ── Input history ──
    pub history: Vec<String>,
    /// `Some(idx)` while navigating history; `None` when at the current draft.
    pub history_index: Option<usize>,
    /// Saved copy of the in-progress input before history navigation started.
    pub(super) draft_input: String,

    // ── Thread picker overlay ──
    pub thread_picker: Option<super::messages::ThreadPicker>,

    // ── Conversation auto-save ──
    /// Thread name for auto-save, set from the first user message.
    /// `None` until the first message after app start or `/new`.
    pub conversation_title: Option<String>,

    // ── Intervention UI state ──
    /// Index of the currently highlighted option while an intervention
    /// prompt is pending. `None` when no intervention is active.
    pub intervene_selection: Option<usize>,

    // ── Exit signal ──
    pub should_quit: bool,
}

impl App {
    /// Creates a fresh app with a welcome system message.
    pub fn new(
        model: impl Into<String>,
        memory: SharedMemory,
        tool_names: Vec<String>,
        workspace_root: PathBuf,
    ) -> Self {
        let model = model.into();
        Self {
            messages: vec![ChatMessage::System {
                content: format!("Loomis — Model: {model} | /help for commands"),
                timestamp: ChatMessage::now_timestamp(),
            }],
            line_counts: vec![1],
            input: String::new(),
            input_cursor: 0,
            scroll_offset: 0,
            auto_scroll: true,
            streaming: false,
            model,
            memory,
            tool_names,
            workspace_root,
            history: Vec::new(),
            history_index: None,
            draft_input: String::new(),
            thread_picker: None,
            conversation_title: None,
            intervene_selection: None,
            should_quit: false,
        }
    }
}

// ── Event Application ────────────────────────────────────────────────────────────

impl App {
    /// Streams an [`AgentEvent`] into the display state.
    ///
    /// This is called from the main event loop via `try_recv` — it processes
    /// events faster than the render frame rate, so the display stays current.
    pub fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Token(text) => match self.messages.last_mut() {
                Some(ChatMessage::Assistant { content, .. }) => {
                    content.push_str(&text);
                }
                _ => {
                    self.messages.push(ChatMessage::Assistant {
                        content: text,
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
            },

            AgentEvent::ReasoningToken(text) => match self.messages.last_mut() {
                Some(ChatMessage::Reasoning { content, .. }) => {
                    content.push_str(&text);
                }
                _ => {
                    self.messages.push(ChatMessage::Reasoning {
                        content: text,
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
            },

            AgentEvent::ToolCall {
                id,
                name,
                arguments,
                origin,
            } => {
                self.messages.push(ChatMessage::ToolCall {
                    id,
                    name,
                    args: arguments,
                    state: ToolCallState::Running,
                    origin,
                    progress_line: None,
                    timestamp: ChatMessage::now_timestamp(),
                });
            }

            AgentEvent::ToolResult { id, output, .. } => {
                for msg in self.messages.iter_mut().rev() {
                    if let ChatMessage::ToolCall { id: mid, state, .. } = msg
                        && *mid == id
                    {
                        *state = ToolCallState::Complete(output);
                        break;
                    }
                }
                // Tool results mean we're still between tool calls —
                // keep `streaming = true` so the loop looks correct.
            }

            AgentEvent::ToolProgress { id, message, .. } => {
                for msg in self.messages.iter_mut().rev() {
                    if let ChatMessage::ToolCall {
                        id: mid,
                        state,
                        progress_line,
                        ..
                    } = msg
                        && *mid == id
                        && matches!(state, ToolCallState::Running)
                    {
                        *progress_line = Some(message);
                        break;
                    }
                }
            }

            AgentEvent::Done => {
                self.streaming = false;
            }

            AgentEvent::NeedUserIntervene(req) => {
                self.messages.push(ChatMessage::Intervene {
                    request_id: req.request_id,
                    title: req.title,
                    description: req.description,
                    options: req.options,
                    responded: false,
                    chosen: None,
                    custom_text: None,
                    timestamp: ChatMessage::now_timestamp(),
                });
            }
        }

        // Auto-scroll to bottom when new content arrives and user hasn't
        // manually scrolled up.
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::messages::TuiCommand;
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use engine::CallOrigin;

    fn ts() -> String {
        "00:00:00".into()
    }

    fn make_app() -> App {
        let memory = std::sync::Arc::new(std::sync::RwLock::new(memory::Memory::new()));
        App::new(
            "test-model",
            memory,
            vec!["echo".into(), "ls".into()],
            PathBuf::from("."),
        )
    }

    // ── apply_event ─────────────────────────────────────────────

    #[test]
    fn test_apply_token_creates_assistant() {
        let mut app = make_app();
        // clear the welcome message for clean test state
        app.messages.clear();
        app.apply_event(AgentEvent::Token("Hello".into()));
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::Assistant { content, .. } => assert_eq!(content, "Hello"),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_token_appends_to_last_assistant() {
        let mut app = make_app();
        app.messages.clear();
        app.apply_event(AgentEvent::Token("Hel".into()));
        app.apply_event(AgentEvent::Token("lo".into()));
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::Assistant { content, .. } => assert_eq!(content, "Hello"),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_token_new_assistant_after_tool_call() {
        let mut app = make_app();
        app.messages.clear();
        app.apply_event(AgentEvent::Token("Before".into()));
        app.apply_event(AgentEvent::ToolCall {
            id: "t1".into(),
            name: "echo".into(),
            arguments: r#"{"x":1}"#.into(),
            origin: CallOrigin::Llm,
        });
        app.apply_event(AgentEvent::ToolResult {
            id: "t1".into(),
            name: "echo".into(),
            output: "ok".into(),
        });
        // New token after tool result creates a fresh Assistant message
        app.apply_event(AgentEvent::Token("After".into()));

        assert_eq!(app.messages.len(), 3); // Before, ToolCall, After
        match &app.messages[0] {
            ChatMessage::Assistant { content, .. } => assert_eq!(content, "Before"),
            other => panic!("expected Assistant, got {other:?}"),
        }
        match &app.messages[2] {
            ChatMessage::Assistant { content, .. } => assert_eq!(content, "After"),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_done_sets_streaming_false() {
        let mut app = make_app();
        app.streaming = true;
        app.apply_event(AgentEvent::Done);
        assert!(!app.streaming);
    }

    #[test]
    fn test_apply_tool_call_lifecycle() {
        let mut app = make_app();
        app.messages.clear();
        app.apply_event(AgentEvent::ToolCall {
            id: "abc".into(),
            name: "ls".into(),
            arguments: r#"{"path":"."}"#.into(),
            origin: CallOrigin::Llm,
        });
        app.apply_event(AgentEvent::ToolResult {
            id: "abc".into(),
            name: "ls".into(),
            output: "src/\nCargo.toml".into(),
        });

        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::ToolCall {
                id,
                name,
                args,
                state,
                ..
            } => {
                assert_eq!(id, "abc");
                assert_eq!(name, "ls");
                assert_eq!(args, r#"{"path":"."}"#);
                match state {
                    ToolCallState::Complete(out) => assert_eq!(out, "src/\nCargo.toml"),
                    ToolCallState::Running => panic!("expected Complete"),
                }
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_reasoning_token() {
        let mut app = make_app();
        app.messages.clear();
        app.apply_event(AgentEvent::ReasoningToken("Hmm, ".into()));
        app.apply_event(AgentEvent::ReasoningToken("let me think...".into()));
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::Reasoning { content, .. } => {
                assert_eq!(content, "Hmm, let me think...");
            }
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    // ── handle_key ──────────────────────────────────────────────

    #[test]
    fn test_slash_exit_returns_exit_command() {
        let mut app = make_app();
        app.input = "/exit".into();
        app.input_cursor = 5;

        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::Exit)));
        assert!(app.should_quit);
    }

    #[test]
    fn test_slash_new_returns_clear_command() {
        let mut app = make_app();
        app.messages.push(ChatMessage::User {
            content: "old".into(),
            timestamp: ts(),
        });
        app.input = "/new".into();
        app.input_cursor = 4;

        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::ClearConversation)));
        // Local messages cleared, replaced with system confirmation
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("New conversation"));
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn test_slash_stats_returns_none() {
        let mut app = make_app();
        app.input = "/stats".into();
        app.input_cursor = 6;

        let result = submit_via_enter(&mut app);
        assert!(result.is_none()); // handled locally
        // welcome message + stats response
        assert_eq!(app.messages.len(), 2);
        match &app.messages[1] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("Messages"));
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn test_normal_message_returns_run_agent() {
        let mut app = make_app();
        app.input = "hello".into();
        app.input_cursor = 5;

        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::RunAgent(msg)) if msg == "hello"));
        assert!(app.streaming);
        // Input cleared
        assert!(app.input.is_empty());
        assert_eq!(app.input_cursor, 0);
        // welcome message + user message
        assert_eq!(app.messages.len(), 2);
        match &app.messages[1] {
            ChatMessage::User { content, .. } => assert_eq!(content, "hello"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn test_ctrl_c_while_streaming_cancels() {
        let mut app = make_app();
        app.streaming = true;
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = app.handle_key(key);
        assert!(matches!(result, Some(TuiCommand::CancelGeneration)));
        assert!(!app.streaming);
    }

    #[test]
    fn test_ctrl_c_while_idle_exits() {
        let mut app = make_app();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = app.handle_key(key);
        assert!(matches!(result, Some(TuiCommand::Exit)));
        assert!(app.should_quit);
    }

    #[test]
    fn test_esc_while_streaming_cancels() {
        let mut app = make_app();
        app.streaming = true;
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let result = app.handle_key(key);
        assert!(matches!(result, Some(TuiCommand::CancelGeneration)));
        assert!(!app.streaming);
    }

    #[test]
    fn test_page_up_increases_scroll_offset() {
        let mut app = make_app();
        app.scroll_offset = 0;
        app.auto_scroll = true;
        let key = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.scroll_offset, 8);
        assert!(!app.auto_scroll);
    }

    #[test]
    fn test_page_down_decreases_scroll_offset() {
        let mut app = make_app();
        app.scroll_offset = 10;
        app.auto_scroll = false;
        let key = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.scroll_offset, 2);
        assert!(!app.auto_scroll);
    }

    #[test]
    fn test_page_down_to_zero_reenables_autoscroll() {
        let mut app = make_app();
        app.scroll_offset = 4;
        app.auto_scroll = false;
        let key = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.scroll_offset, 0);
        assert!(app.auto_scroll);
    }

    #[test]
    fn test_character_insertion() {
        let mut app = make_app();
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.input, "x");
        assert_eq!(app.input_cursor, 1);
    }

    #[test]
    fn test_backspace_deletes() {
        let mut app = make_app();
        app.input = "ab".into();
        app.input_cursor = 1;
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.input, "b");
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn test_left_right_movement() {
        let mut app = make_app();
        app.input = "abc".into();
        app.input_cursor = 1;

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.input_cursor, 2);

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.input_cursor, 1);
    }

    // ── Bang command tests ──────────────────────────────────────

    #[test]
    fn test_bang_command_returns_run_shell() {
        let mut app = make_app();
        app.input = "!echo hello".into();
        app.input_cursor = 11;

        let result = submit_via_enter(&mut app);
        assert!(
            matches!(result, Some(TuiCommand::RunShell(ref cmd)) if cmd == "echo hello"),
            "expected RunShell(\"echo hello\"), got {result:?}"
        );
        assert!(!app.streaming);
        assert!(app.input.is_empty());
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn test_apply_shell_output_creates_message() {
        let mut app = make_app();
        app.messages.clear();
        // User !command now uses unified ToolCall with origin: User
        app.apply_event(AgentEvent::ToolCall {
            id: "shell-1".into(),
            name: "shell".into(),
            arguments: "echo test".into(),
            origin: CallOrigin::User,
        });
        app.apply_event(AgentEvent::ToolResult {
            id: "shell-1".into(),
            name: "shell".into(),
            output: "test".into(),
        });
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::ToolCall {
                name,
                args,
                state,
                origin,
                ..
            } => {
                assert_eq!(name, "shell");
                assert_eq!(args, "echo test");
                assert!(matches!(origin, CallOrigin::User));
                match state {
                    ToolCallState::Complete(output) => assert!(output.contains("test")),
                    ToolCallState::Running => panic!("expected Complete"),
                }
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn test_bang_empty_command_shows_help() {
        let mut app = make_app();
        app.messages.clear();
        app.input = "!".into();
        app.input_cursor = 1;

        let result = submit_via_enter(&mut app);
        assert!(result.is_none());
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("Usage"), "got: {content}");
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn test_double_bang_not_treated_as_command() {
        let mut app = make_app();
        app.messages.clear();
        app.input = "!!echo".into();
        app.input_cursor = 6;

        let result = submit_via_enter(&mut app);
        // !! should be treated as a normal message, triggering RunAgent
        assert!(matches!(result, Some(TuiCommand::RunAgent(_))));
    }

    #[test]
    fn test_bang_command_whitespace_only() {
        let mut app = make_app();
        app.messages.clear();
        app.input = "!   ".into();
        app.input_cursor = 4;

        let result = submit_via_enter(&mut app);
        assert!(result.is_none());
        // Empty command after trimming should show usage hint
        match &app.messages[0] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("Usage"), "got: {content}");
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    // ── Test Helpers ────────────────────────────────────────────

    /// Simulates Enter: calls handle_key with Enter, returns the command.
    fn submit_via_enter(app: &mut App) -> Option<TuiCommand> {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_key(key)
    }
}
