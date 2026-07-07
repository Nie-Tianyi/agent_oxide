//! # App State
//!
//! The mutable state machine for the TUI: chat messages, input buffer,
//! scrolling, streaming status, and keyboard processing.

use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::core::agent::AgentEvent;
use crate::memory::SharedMemory;

// ── ChatMessage ──────────────────────────────────────────────────────────────────

/// One display entry in the chat area.
///
/// Each variant maps to a distinct visual style in the UI
/// (see [`super::ui`] for the rendering).
#[derive(Debug, Clone)]
pub enum ChatMessage {
    /// User input — cyan, bold, `>` prefix.
    User { content: String, timestamp: String },
    /// Model text output — white, no prefix. Streamed token-by-token.
    Assistant { content: String, timestamp: String },
    /// Chain-of-thought reasoning — yellow, dimmed.
    Reasoning { content: String, timestamp: String },
    /// A tool call, either in-progress or completed.
    ToolCall {
        id: String,
        name: String,
        args: String,
        state: ToolCallState,
        timestamp: String,
    },
    /// System-level message (slash commands, info).
    System { content: String, timestamp: String },
    /// Error display — red, bold.
    Error { content: String, timestamp: String },
}

impl ChatMessage {
    /// Returns a formatted local-time timestamp string (HH:MM:SS).
    pub fn now_timestamp() -> String {
        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Approximate local time from UTC — works for display purposes.
        // On Windows this gives UTC; use a simple offset heuristic.
        let total_secs = secs % 86400; // seconds within the day
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        let seconds = total_secs % 60;
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    }
}

#[derive(Debug, Clone)]
pub enum ToolCallState {
    /// Arguments are still streaming in.
    Running,
    /// Tool execution finished with this output.
    Complete(String),
}

// ── TuiCommand ───────────────────────────────────────────────────────────────────

/// Commands sent from the TUI thread to the agent background task.
#[derive(Debug, Clone)]
pub enum TuiCommand {
    /// User submitted a message — push to memory and run the agent loop.
    RunAgent(String),
    /// Cancel the currently-running generation.
    CancelGeneration,
    /// Reset conversation, preserving system prompt.
    ClearConversation,
    /// Signal the agent thread to exit.
    Exit,
}

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

    // ── Input history ──
    pub history: Vec<String>,
    /// `Some(idx)` while navigating history; `None` when at the current draft.
    pub history_index: Option<usize>,
    /// Saved copy of the in-progress input before history navigation started.
    draft_input: String,

    // ── Exit signal ──
    pub should_quit: bool,
}

impl App {
    /// Creates a fresh app with a welcome system message.
    pub fn new(model: impl Into<String>, memory: SharedMemory, tool_names: Vec<String>) -> Self {
        let model = model.into();
        Self {
            messages: vec![ChatMessage::System {
                content: format!("Agent Oxide — Model: {model} | /help for commands"),
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
            history: Vec::new(),
            history_index: None,
            draft_input: String::new(),
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

            AgentEvent::ToolCallStart { id, name } => {
                self.messages.push(ChatMessage::ToolCall {
                    id,
                    name,
                    args: String::new(),
                    state: ToolCallState::Running,
                    timestamp: ChatMessage::now_timestamp(),
                });
            }

            AgentEvent::ToolCallArgsDelta { id, delta } => {
                for msg in self.messages.iter_mut().rev() {
                    if let ChatMessage::ToolCall { id: mid, args, .. } = msg
                        && *mid == id
                    {
                        args.push_str(&delta);
                        break;
                    }
                }
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

            AgentEvent::Done => {
                self.streaming = false;
            }
        }

        // Auto-scroll to bottom when new content arrives and user hasn't
        // manually scrolled up.
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
    }
}

// ── Keyboard Handling ────────────────────────────────────────────────────────────

impl App {
    /// Processes a single key event. Returns `Some(TuiCommand)` when the
    /// key sequence triggers an action that needs the agent thread.
    ///
    /// Slash commands (`/stats`, `/tools`) are handled inline because they
    /// only need shared state already available on the TUI side.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        match key.code {
            // ── Submit / Newline ───────────────────────────────
            KeyCode::Enter => {
                // Shift+Enter inserts a newline
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert(self.input_cursor, '\n');
                    self.input_cursor += 1;
                    return None;
                }

                if self.streaming {
                    return None;
                }

                let input = self.input.trim().to_string();
                if input.is_empty() {
                    return None;
                }

                // Save to history
                self.history.push(input.clone());
                self.history_index = None;
                self.draft_input.clear();

                // Check for slash commands
                if let Some(cmd) = self.handle_slash_command(&input) {
                    self.input.clear();
                    self.input_cursor = 0;
                    self.auto_scroll = true;
                    return cmd;
                }

                // Normal user message
                self.messages.push(ChatMessage::User {
                    content: input.clone(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                self.input.clear();
                self.input_cursor = 0;
                self.auto_scroll = true;
                self.scroll_offset = 0;
                self.streaming = true;

                Some(TuiCommand::RunAgent(input))
            }

            // ── Exit / Cancel ──────────────────────────────────
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.streaming {
                    self.streaming = false;
                    return Some(TuiCommand::CancelGeneration);
                }
                self.should_quit = true;
                Some(TuiCommand::Exit)
            }

            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.input.is_empty() && !self.streaming {
                    self.should_quit = true;
                    return Some(TuiCommand::Exit);
                }
                // Otherwise: delete forward
                self.delete_at_cursor();
                None
            }

            KeyCode::Esc => {
                if self.streaming {
                    self.streaming = false;
                    return Some(TuiCommand::CancelGeneration);
                }
                None
            }

            // ── Scrolling ──────────────────────────────────────
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(8);
                self.auto_scroll = false;
                None
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(8);
                if self.scroll_offset == 0 {
                    self.auto_scroll = true;
                }
                None
            }

            // ── Multi-line / History navigation ────────────────
            KeyCode::Up => {
                // If not navigating history and cursor is below first line,
                // move cursor up within multi-line input.
                if self.history_index.is_none() {
                    let cursor_line = self.input[..self.input_cursor]
                        .chars()
                        .filter(|&c| c == '\n')
                        .count();
                    if cursor_line > 0 {
                        // Find the start of the current line
                        let line_start = self.input[..self.input_cursor]
                            .rfind('\n')
                            .map(|p| p + 1)
                            .unwrap_or(0);
                        // Find the start of the previous line
                        if let Some(prev_start) =
                            self.input[..line_start.saturating_sub(1)].rfind('\n')
                        {
                            let prev_start = prev_start + 1;
                            let prev_line_len = line_start.saturating_sub(prev_start + 1);
                            // Position cursor at same column, clamped to line length
                            let col_in_line = self.input_cursor.saturating_sub(line_start);
                            let new_col = col_in_line.min(prev_line_len);
                            self.input_cursor = prev_start + new_col;
                        } else {
                            // First line — column clamped
                            let col_in_line = self.input_cursor.saturating_sub(line_start);
                            let new_col = col_in_line.min(line_start.saturating_sub(1));
                            self.input_cursor = new_col;
                        }
                        return None;
                    }
                }

                // Fall through to history navigation
                if self.history.is_empty() {
                    return None;
                }
                if self.history_index.is_none() {
                    self.draft_input = self.input.clone();
                    self.history_index = Some(self.history.len());
                }
                if let Some(ref mut idx) = self.history_index
                    && *idx > 0
                {
                    *idx -= 1;
                    self.input = self.history[*idx].clone();
                    self.input_cursor = self.input.len();
                }
                None
            }
            KeyCode::Down => {
                // If not navigating history, try to move cursor down in multi-line input.
                if self.history_index.is_none() {
                    let total_lines = self.input.chars().filter(|&c| c == '\n').count() + 1;
                    let cursor_line = self.input[..self.input_cursor]
                        .chars()
                        .filter(|&c| c == '\n')
                        .count();
                    if cursor_line + 1 < total_lines {
                        // Find end of current line
                        let line_end = self.input[self.input_cursor..]
                            .find('\n')
                            .map(|p| self.input_cursor + p)
                            .unwrap_or(self.input.len());
                        // Start of next line
                        let next_line_start = line_end + 1;
                        // End of next line
                        let next_line_end = self.input[next_line_start..]
                            .find('\n')
                            .map(|p| next_line_start + p)
                            .unwrap_or(self.input.len());
                        // Position at same column, clamped to next line length
                        let line_start = self.input[..self.input_cursor]
                            .rfind('\n')
                            .map(|p| p + 1)
                            .unwrap_or(0);
                        let col_in_line = self.input_cursor.saturating_sub(line_start);
                        let next_line_len = next_line_end.saturating_sub(next_line_start);
                        let new_col = col_in_line.min(next_line_len);
                        self.input_cursor = next_line_start + new_col;
                        return None;
                    }
                }

                // Fall through to history navigation
                if let Some(ref mut idx) = self.history_index {
                    if *idx + 1 < self.history.len() {
                        *idx += 1;
                        self.input = self.history[*idx].clone();
                    } else {
                        // End of history — restore draft
                        self.history_index = None;
                        self.input = self.draft_input.clone();
                    }
                    self.input_cursor = self.input.len();
                }
                None
            }

            // ── Cursor movement ────────────────────────────────
            KeyCode::Home => {
                self.input_cursor = 0;
                None
            }
            KeyCode::End => {
                self.input_cursor = self.input.len();
                None
            }
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor = self.prev_char_boundary();
                }
                None
            }
            KeyCode::Right => {
                if self.input_cursor < self.input.len() {
                    self.input_cursor = self.next_char_boundary();
                }
                None
            }

            // ── Editing ────────────────────────────────────────
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    let prev = self.prev_char_boundary();
                    self.input.remove(prev);
                    self.input_cursor = prev;
                }
                None
            }
            KeyCode::Delete => {
                if self.input_cursor < self.input.len() {
                    self.delete_at_cursor();
                }
                None
            }

            // ── Character insertion ────────────────────────────
            KeyCode::Char(c) => {
                if self.streaming {
                    return None;
                }
                // On some terminals Shift+Enter sends a newline char
                // (handled above via Enter). Plain char insertion:
                self.input.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();
                None
            }

            _ => None,
        }
    }
}

// ── Slash Commands ───────────────────────────────────────────────────────────────

impl App {
    /// Handles slash commands that don't need the agent. Returns
    /// `Some(TuiCommand)` when the command needs agent-thread action.
    fn handle_slash_command(&mut self, input: &str) -> Option<Option<TuiCommand>> {
        match input {
            "/exit" => {
                self.should_quit = true;
                Some(Some(TuiCommand::Exit))
            }

            "/clear" => {
                self.messages.clear();
                self.messages.push(ChatMessage::System {
                    content: "Conversation cleared (system prompt preserved).".into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(Some(TuiCommand::ClearConversation))
            }

            "/stats" => {
                let mem = self.memory.read().unwrap();
                let content = format!(
                    "Messages: {}  |  Characters: {}  |  Threshold: {}  |  Keep last: {}",
                    mem.message_count(),
                    mem.total_chars(),
                    mem.compact_threshold(),
                    mem.keep_last_n(),
                );
                self.messages.push(ChatMessage::System {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(None) // handled locally
            }

            "/tools" => {
                let content = if self.tool_names.is_empty() {
                    "No tools registered.".to_string()
                } else {
                    self.tool_names
                        .iter()
                        .enumerate()
                        .map(|(i, name)| format!("  {}. {}", i + 1, name))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                self.messages.push(ChatMessage::System {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(None) // handled locally
            }

            "/help" => {
                let content = [
                    "Commands:",
                    "  /exit   — quit",
                    "  /clear  — reset conversation",
                    "  /stats  — memory statistics",
                    "  /tools  — list registered tools",
                    "  /help   — show this message",
                    "",
                    "Keys:",
                    "  Enter        — send message",
                    "  Shift+Enter  — newline",
                    "  PgUp/PgDown  — scroll chat",
                    "  Up/Down      — input history / multi-line nav",
                    "  Ctrl+C       — cancel generation / exit",
                    "  Esc          — cancel generation",
                ]
                .join("\n");
                self.messages.push(ChatMessage::System {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(None)
            }

            _ => None, // not a slash command — normal message
        }
    }
}

// ── Unicode-safe editing helpers ─────────────────────────────────────────────────

impl App {
    /// Returns the byte position of the previous UTF-8 char boundary.
    fn prev_char_boundary(&self) -> usize {
        if self.input_cursor == 0 {
            return 0;
        }
        let mut pos = self.input_cursor - 1;
        while !self.input.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    }

    /// Returns the byte position of the next UTF-8 char boundary.
    fn next_char_boundary(&self) -> usize {
        if self.input_cursor >= self.input.len() {
            return self.input.len();
        }
        let mut pos = self.input_cursor + 1;
        while pos < self.input.len() && !self.input.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    }

    /// Deletes the character at (after) the cursor position.
    fn delete_at_cursor(&mut self) {
        if self.input_cursor < self.input.len() {
            let next = self.next_char_boundary();
            self.input.drain(self.input_cursor..next);
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> String {
        "00:00:00".into()
    }

    fn make_app() -> App {
        let memory = std::sync::Arc::new(std::sync::RwLock::new(crate::memory::Memory::new()));
        App::new("test-model", memory, vec!["echo".into(), "ls".into()])
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
        app.apply_event(AgentEvent::ToolCallStart {
            id: "t1".into(),
            name: "echo".into(),
        });
        app.apply_event(AgentEvent::ToolCallArgsDelta {
            id: "t1".into(),
            delta: r#"{"x":1}"#.into(),
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
        app.apply_event(AgentEvent::ToolCallStart {
            id: "abc".into(),
            name: "ls".into(),
        });
        app.apply_event(AgentEvent::ToolCallArgsDelta {
            id: "abc".into(),
            delta: r#"{"path""#.into(),
        });
        app.apply_event(AgentEvent::ToolCallArgsDelta {
            id: "abc".into(),
            delta: r#":"."}"#.into(),
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
    fn test_slash_clear_returns_clear_command() {
        let mut app = make_app();
        app.messages.push(ChatMessage::User {
            content: "old".into(),
            timestamp: ts(),
        });
        app.input = "/clear".into();
        app.input_cursor = 6;

        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::ClearConversation)));
        // Local messages cleared, replaced with system confirmation
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("cleared"));
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

    // ── Test Helpers ────────────────────────────────────────────

    /// Simulates Enter: calls handle_key with Enter, returns the command.
    fn submit_via_enter(app: &mut App) -> Option<TuiCommand> {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_key(key)
    }
}
