//! Keyboard handling, slash commands, thread picker, and shell confirmation.
//!
//! All input processing that was previously in the monolithic [`super::app`].
//! These are [`super::app::App`] methods split into their own file for readability.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use provider::Role;

use super::app::App;
use super::messages::{ChatMessage, TuiCommand, is_valid_thread_name, truncate_for_display};

// ── Keyboard Handling ────────────────────────────────────────────────────────────

impl App {
    /// Processes a single key event. Returns `Some(TuiCommand)` when the
    /// key sequence triggers an action that needs the agent thread.
    ///
    /// Slash commands (`/stats`, `/tools`) are handled inline because they
    /// only need shared state already available on the TUI side.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        // ── Thread picker intercepts most keys ───────────────────
        if self.thread_picker.is_some() {
            return self.handle_thread_picker_key(key);
        }

        // ── Intervention prompt intercepts most keys ────────────
        if self.has_pending_intervene() {
            return self.handle_intervene_key(key);
        }

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

                // Check for bang commands (!command — execute asynchronously)
                if input.starts_with('!') && !input.starts_with("!!") {
                    let command = input[1..].trim().to_string();
                    self.input.clear();
                    self.input_cursor = 0;
                    self.auto_scroll = true;
                    if command.is_empty() {
                        self.messages.push(ChatMessage::System {
                            content: "Usage: !<command> — runs a shell command and shares output with the agent."
                                .into(),
                            timestamp: ChatMessage::now_timestamp(),
                        });
                        return None;
                    }
                    return Some(TuiCommand::RunShell(command));
                }

                // Check for slash commands
                if let Some(cmd) = self.handle_slash_command(&input) {
                    self.input.clear();
                    self.input_cursor = 0;
                    self.auto_scroll = true;
                    return cmd;
                }

                // Normal user message — generate auto-save thread name from
                // the first message of this conversation.
                if self.conversation_title.is_none() {
                    let title = memory::generate_thread_name(&input);
                    let _ = memory::write_current_thread(&title, &self.workspace_root);
                    self.conversation_title = Some(title);
                }

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
                // If there's a pending intervention prompt, route to
                // the intervention key handler.
                if self.has_pending_intervene() {
                    return self.handle_intervene_key(key);
                }

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
        // ── Prefix commands (have arguments) ──
        if let Some(name) = input.strip_prefix("/save ") {
            let name = name.trim();
            if name.is_empty() || !is_valid_thread_name(name) {
                self.messages.push(ChatMessage::System {
                    content: "Usage: /save <name>  —  name must use only letters, digits, hyphens, and underscores.".into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                return Some(None);
            }
            let mem = self.memory.read().unwrap();
            match memory::save_conversation(name, &self.workspace_root, &mem) {
                Ok(()) => {
                    let _ = memory::write_current_thread(name, &self.workspace_root);
                    self.messages.push(ChatMessage::System {
                        content: format!("Saved conversation as \"{name}\"."),
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
                Err(e) => {
                    self.messages.push(ChatMessage::Error {
                        content: format!("Failed to save: {e}"),
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
            }
            return Some(None);
        }

        if let Some(name) = input.strip_prefix("/resume ") {
            let name = name.trim();
            if name.is_empty() {
                self.open_thread_picker();
                return Some(None);
            }
            return Some(self.do_resume(name));
        }

        // ── Exact-match commands ──
        match input {
            "/exit" => {
                self.should_quit = true;
                Some(Some(TuiCommand::Exit))
            }

            "/new" => {
                // Save current conversation before starting fresh.
                if let Some(ref title) = self.conversation_title {
                    let mem = self.memory.read().unwrap();
                    let _ = memory::save_conversation(title, &self.workspace_root, &mem);
                }
                self.conversation_title = None;
                // Write fallback for the gap between /new and first message.
                let _ = memory::write_current_thread("autosave", &self.workspace_root);

                self.messages.clear();
                self.messages.push(ChatMessage::System {
                    content: "New conversation started (system prompt preserved).".into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(Some(TuiCommand::ClearConversation))
            }

            "/resume" | "/threads" => {
                self.open_thread_picker();
                Some(None)
            }

            "/stats" => {
                let mem = self.memory.read().unwrap();
                let content = format!(
                    "Messages: {}  |  Characters: {}",
                    mem.message_count(),
                    mem.total_chars(),
                );
                self.messages.push(ChatMessage::System {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(None)
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
                Some(None)
            }

            "/help" => {
                let content = [
                    "Commands:",
                    "  /exit          — quit",
                    "  /new           — start a new conversation",
                    "  /save <name>   — save conversation as a named thread",
                    "  /resume [name] — restore a thread (no name = picker)",
                    "  /threads       — open thread picker",
                    "  /stats         — memory statistics",
                    "  /tools         — list registered tools",
                    "  /help          — show this message",
                    "",
                    "Shell prefix:",
                    "  !<cmd>  — run a shell command and share output with the agent",
                    "  !!text  — literal text starting with '!' (not a shell command)",
                    "  Example: !dir, !git status, !cargo test",
                    "",
                    "Keys:",
                    "  Enter        — send message",
                    "  Shift+Enter  — newline",
                    "  PgUp/PgDown/🖱 — scroll chat",
                    "  Up/Down      — input history / multi-line nav",
                    "  Ctrl+C       — cancel generation / exit",
                    "  Esc          — cancel generation",
                    "  Y / n        — approve / deny shell command",
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

    // ── Thread Picker ─────────────────────────────────────────────────────────

    /// Handles keyboard input while the thread picker overlay is active.
    ///
    /// Only `Esc`, `Enter`, `Up`, and `Down` are processed; all other keys
    /// are swallowed to prevent input from leaking into the chat.
    fn handle_thread_picker_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        let Some(picker) = &mut self.thread_picker else {
            return None;
        };

        match key.code {
            KeyCode::Esc => {
                self.thread_picker = None;
                None
            }
            KeyCode::Enter => {
                let name = picker.threads[picker.selected].name.clone();
                self.thread_picker = None;
                self.do_resume(&name)
            }
            KeyCode::Up => {
                if picker.selected > 0 {
                    picker.selected -= 1;
                }
                None
            }
            KeyCode::Down => {
                if picker.selected + 1 < picker.threads.len() {
                    picker.selected += 1;
                }
                None
            }
            _ => None, // swallow all other keys
        }
    }

    /// Loads a named thread and replaces the current conversation.
    ///
    /// Shared by the picker (`Enter`) and the `/resume <name>` slash command.
    fn do_resume(&mut self, name: &str) -> Option<TuiCommand> {
        match memory::load_conversation(name, &self.workspace_root) {
            Ok(loaded) => {
                *self.memory.write().unwrap() = loaded;
                let _ = memory::write_current_thread(name, &self.workspace_root);
                self.conversation_title = Some(name.to_string());
                self.rebuild_messages_from_memory();
                self.messages.insert(
                    0,
                    ChatMessage::System {
                        content: format!("Resumed conversation \"{name}\"."),
                        timestamp: ChatMessage::now_timestamp(),
                    },
                );
            }
            Err(e) => {
                self.messages.push(ChatMessage::Error {
                    content: format!("Failed to resume \"{name}\": {e}"),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }
        }
        None
    }

    /// Opens the thread picker overlay with all saved conversations.
    fn open_thread_picker(&mut self) {
        match memory::list_threads(&self.workspace_root) {
            Ok(threads) if !threads.is_empty() => {
                self.thread_picker = Some(super::messages::ThreadPicker {
                    threads,
                    selected: 0,
                });
            }
            Ok(_) => {
                self.messages.push(ChatMessage::System {
                    content: "No saved conversations. Use /save <name> to save one.".into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }
            Err(e) => {
                self.messages.push(ChatMessage::Error {
                    content: format!("Error listing threads: {e}"),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }
        }
    }

    /// Rebuilds `self.messages` (TUI display) from the current state of
    /// `self.memory`. Used after `/resume` to restore display history.
    fn rebuild_messages_from_memory(&mut self) {
        let mem = self.memory.read().unwrap();
        let msgs = mem.messages().to_vec(); // clone under lock, then drop
        drop(mem);

        let ts = ChatMessage::now_timestamp();
        self.messages.clear();

        for msg in &msgs {
            match msg.role {
                Role::System => {
                    self.messages.push(ChatMessage::System {
                        content: msg.content.clone(),
                        timestamp: ts.clone(),
                    });
                }
                Role::User => {
                    self.messages.push(ChatMessage::User {
                        content: msg.content.clone(),
                        timestamp: ts.clone(),
                    });
                }
                Role::Assistant => {
                    // Append compact tool-call summary if present
                    let content = if let Some(ref tool_calls) = msg.tool_calls {
                        let tc_list: Vec<String> = tool_calls
                            .iter()
                            .map(|tc| format!("[Tool: {} (id: {})]", tc.function.name, tc.id))
                            .collect();
                        if msg.content.is_empty() {
                            tc_list.join("\n")
                        } else {
                            format!("{}\n\n{}", msg.content, tc_list.join("\n"))
                        }
                    } else {
                        msg.content.clone()
                    };
                    self.messages.push(ChatMessage::Assistant {
                        content,
                        timestamp: ts.clone(),
                    });
                }
                Role::Tool => {
                    let preview = truncate_for_display(&msg.content, 500);
                    let id = msg.tool_call_id.as_deref().unwrap_or("?");
                    self.messages.push(ChatMessage::System {
                        content: format!("[Tool result: {id}]\n{preview}"),
                        timestamp: ts.clone(),
                    });
                }
            }
        }

        self.auto_scroll = true;
        self.scroll_offset = 0;
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

// ── Intervention Helpers ──────────────────────────────────────────────────────────

impl App {
    /// Returns `true` if there is an unresponded [`ChatMessage::Intervene`]
    /// in the message list.
    fn has_pending_intervene(&self) -> bool {
        self.messages.iter().rev().any(|msg| {
            matches!(
                msg,
                ChatMessage::Intervene {
                    responded: false,
                    ..
                }
            )
        })
    }

    /// Routes all key presses while an intervention prompt is active.
    fn handle_intervene_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        // Lazy-init the selection to the first option.
        let (options_len, _responded) = self.intervene_state();
        if self.intervene_selection.is_none() || self.intervene_selection.unwrap() >= options_len {
            self.intervene_selection = Some(0);
        }

        match key.code {
            KeyCode::Up => {
                if let Some(sel) = self.intervene_selection.as_mut() {
                    *sel = sel.saturating_sub(1);
                }
                None
            }
            KeyCode::Down => {
                if let Some(sel) = self.intervene_selection.as_mut() {
                    *sel = (*sel + 1).min(options_len.saturating_sub(1));
                }
                None
            }
            KeyCode::Enter => {
                let sel = self.intervene_selection.unwrap_or(0);
                let options: Vec<String> = self
                    .messages
                    .iter()
                    .rev()
                    .find_map(|msg| match msg {
                        ChatMessage::Intervene {
                            responded: false,
                            options,
                            ..
                        } => Some(options.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let chosen_label = options.get(sel).cloned().unwrap_or_default();

                // If the chosen option ends with "…", enter text-input mode.
                // For now, handle as approve with the label text.
                // Future: implement proper text-input dialog.
                let custom_text = if chosen_label.ends_with('…') {
                    // Use the existing input buffer as the custom text source.
                    // The user can type their response in the input area.
                    let text = self.input.clone();
                    self.input.clear();
                    self.input_cursor = 0;
                    Some(text)
                } else {
                    None
                };

                self.complete_intervene(Some(sel), custom_text)
            }
            KeyCode::Esc => {
                // Cancel — same as deny.
                self.complete_intervene(None, None)
            }
            KeyCode::Char(c) => {
                // If the selected option ends with "…", pass chars
                // through to the input buffer for custom text entry.
                let is_other = self.is_intervene_other_option_selected();
                if is_other {
                    self.input.insert(self.input_cursor, c);
                    self.input_cursor += c.len_utf8();
                    None
                } else {
                    // Quick-select by first character of each option.
                    let c_lower = c.to_ascii_lowercase();
                    let options: Vec<String> = self
                        .messages
                        .iter()
                        .rev()
                        .find_map(|msg| match msg {
                            ChatMessage::Intervene {
                                responded: false,
                                options,
                                ..
                            } => Some(options.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    for (i, opt) in options.iter().enumerate() {
                        if opt.to_ascii_lowercase().starts_with(c_lower) {
                            self.intervene_selection = Some(i);
                            // Auto-confirm on quick-select (pressing first char).
                            let custom = if opt.ends_with('…') {
                                Some(String::new())
                            } else {
                                None
                            };
                            return self.complete_intervene(Some(i), custom);
                        }
                    }
                    None
                }
            }
            _ => None,
        }
    }

    /// Returns `true` if the currently highlighted option ends with `…`.
    fn is_intervene_other_option_selected(&self) -> bool {
        let sel = self.intervene_selection.unwrap_or(0);
        self.messages
            .iter()
            .rev()
            .find_map(|msg| match msg {
                ChatMessage::Intervene {
                    responded: false,
                    options,
                    ..
                } => options.get(sel).map(|o| o.ends_with('…')),
                _ => None,
            })
            .unwrap_or(false)
    }

    /// Returns `(options_len, has_been_responded)` for the pending intervention.
    fn intervene_state(&self) -> (usize, bool) {
        self.messages
            .iter()
            .rev()
            .find_map(|msg| match msg {
                ChatMessage::Intervene {
                    options, responded, ..
                } => Some((options.len(), *responded)),
                _ => None,
            })
            .unwrap_or((0, true))
    }

    /// Marks the last unresponded intervention as completed and returns
    /// the [`TuiCommand::InterveneResponse`].
    fn complete_intervene(
        &mut self,
        chosen: Option<usize>,
        custom_text: Option<String>,
    ) -> Option<TuiCommand> {
        self.intervene_selection = None;
        let response = engine::InterveneResponse {
            chosen,
            custom_text,
        };
        for msg in self.messages.iter_mut().rev() {
            if let ChatMessage::Intervene {
                request_id,
                responded,
                chosen,
                custom_text,
                ..
            } = msg
                && !*responded
            {
                *responded = true;
                *chosen = response.chosen;
                *custom_text = response.custom_text.clone();
                return Some(TuiCommand::InterveneResponse {
                    request_id: request_id.clone(),
                    response,
                });
            }
        }
        None
    }
}
