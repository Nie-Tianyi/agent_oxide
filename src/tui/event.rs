//! # Event Loop
//!
//! Bridges the synchronous ratatui render loop with the async tokio agent.
//! The main thread runs the TUI (render + input), while a background tokio
//! task manages the agent lifecycle (spawn, cancel, clear).
//!
//! ## Channel topology
//!
//! ```text
//! TUI thread                          Agent task (tokio::spawn)
//! ─────────                          ────────────────────────
//! cmd_tx ───────── TuiCommand ──────→ cmd_rx
//! agent_rx ←────── AgentEvent ─────── agent_tx
//! ```

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::core::agent::{Agent, AgentEvent};
use crate::core::client::{Message, Role};
use crate::memory::{Memory, SharedMemory};
use crate::persistence;

use super::app::{App, TuiCommand};

// ── Entry Point ──────────────────────────────────────────────────────────────────

/// Initialises the TUI and runs the event loop until the user exits.
///
/// This function is **synchronous** — it blocks the calling thread until
/// the user types `/exit` or presses Ctrl+C/D. The caller must already be
/// inside a tokio runtime (e.g. via `#[tokio::main]`).
///
/// # Parameters
///
/// - `agent` — the configured [`Agent`] (moved into a background task).
/// - `memory` — shared conversation history.
/// - `tool_names` — cached list of tool names for the `/tools` command.
/// - `model` — model name for the status bar.
pub fn run(
    agent: Agent,
    memory: SharedMemory,
    tool_names: Vec<String>,
    model: &str,
    workspace_root: PathBuf,
) -> io::Result<()> {
    // ── Create channels ──────────────────────────────────────────────
    let (agent_tx, agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();

    // ── Spawn agent handler ─────────────────────────────────────────
    tokio::spawn(agent_handler(
        Arc::new(agent),
        memory.clone(),
        cmd_rx,
        agent_tx,
        workspace_root.clone(),
    ));

    // ── Terminal setup ───────────────────────────────────────────────
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    // Install panic hook to restore terminal on crash
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        prev_hook(info);
    }));

    // ── App state ────────────────────────────────────────────────────
    let mut app = App::new(model, memory, tool_names, workspace_root);

    // ── Event loop ───────────────────────────────────────────────────
    let result = run_event_loop(&mut terminal, &mut app, agent_rx, &cmd_tx);

    // ── Cleanup ──────────────────────────────────────────────────────
    let _ = cmd_tx.send(TuiCommand::Exit);
    terminal.show_cursor()?;
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
    )?;

    // Restore previous panic hook
    let _ = std::panic::take_hook();

    result
}

// ── Event Loop ───────────────────────────────────────────────────────────────────

/// The main TUI loop: poll input, drain agent events, render.
fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    agent_rx: UnboundedReceiver<AgentEvent>,
    cmd_tx: &UnboundedSender<TuiCommand>,
) -> io::Result<()> {
    // We need to poll agent_rx without holding a mutable borrow on app
    // (terminal.draw borrows app immutably). So we collect events first,
    // then apply them.
    let mut agent_rx = agent_rx;
    let mut pending_events: Vec<AgentEvent> = Vec::new();

    loop {
        // ── Render ───────────────────────────────────────────────────
        terminal.draw(|frame| super::ui::draw(frame, app))?;

        // ── Poll keyboard ────────────────────────────────────────────
        if crossterm::event::poll(Duration::from_millis(50))? {
            match crossterm::event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat =>
                {
                    if let Some(cmd) = app.handle_key(key) {
                        match cmd {
                            TuiCommand::Exit => {
                                return Ok(());
                            }
                            cmd => {
                                let _ = cmd_tx.send(cmd);
                            }
                        }
                    }
                }
                Event::Resize(..) => {
                    // ratatui handles resize in terminal.draw(),
                    // but we reset scroll so the viewport doesn't end up
                    // in a weird state.
                    app.scroll_offset = 0;
                    app.auto_scroll = true;
                }
                Event::Mouse(mouse_event) => match mouse_event.kind {
                    MouseEventKind::ScrollUp => {
                        app.scroll_offset = app.scroll_offset.saturating_add(4);
                        app.auto_scroll = false;
                    }
                    MouseEventKind::ScrollDown => {
                        app.scroll_offset = app.scroll_offset.saturating_sub(4);
                        if app.scroll_offset == 0 {
                            app.auto_scroll = true;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        // ── Drain agent events ───────────────────────────────────────
        while let Ok(event) = agent_rx.try_recv() {
            pending_events.push(event);
        }

        // Apply all events together
        for event in pending_events.drain(..) {
            app.apply_event(event);
        }

        // ── Quit signal ──────────────────────────────────────────────
        if app.should_quit {
            return Ok(());
        }
    }
}

// ── Agent Handler ────────────────────────────────────────────────────────────────

/// Background task that processes [`TuiCommand`]s and manages the agent lifecycle.
///
/// The agent is wrapped in an `Arc` so it can be shared into spawned tasks.
/// When a [`TuiCommand::RunAgent`] arrives, we push the user message to memory
/// and spawn a new tokio task that calls `Agent::run_with_events()`. The events
/// flow back to the TUI through `agent_tx`.
///
/// Cancellation is handled via `JoinHandle::abort()`. Since the agent's
/// own `run_streaming_loop` periodically `.await`s (network I/O), abort
/// takes effect quickly.
async fn agent_handler(
    agent: Arc<Agent>,
    memory: SharedMemory,
    mut cmd_rx: UnboundedReceiver<TuiCommand>,
    agent_tx: UnboundedSender<AgentEvent>,
    workspace_root: PathBuf,
) {
    let mut current_run: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            TuiCommand::RunAgent(input) => {
                // If a previous run is still active, cancel it.
                if let Some(h) = current_run.take() {
                    h.abort();
                }

                // Push user message to shared memory.
                {
                    let mut mem = memory.write().unwrap();
                    mem.push(Message::new(Role::User, &input));
                }

                // Spawn the agent in a background task. We clone the
                // sender so the task owns its own — when the task
                // completes, its sender is dropped, freeing the channel
                // for the next run.
                let tx = agent_tx.clone();
                let agent = Arc::clone(&agent);
                let ws = workspace_root.clone();
                let mem_for_save = memory.clone();

                let handle = tokio::spawn(async move {
                    let result = agent.run_with_events(tx.clone()).await;

                    // Auto-save conversation after each agent turn.
                    {
                        let mem = mem_for_save.read().unwrap();
                        let name = persistence::default_thread_name(&ws);
                        let _ = persistence::save_conversation(&name, &ws, &mem);
                    }

                    match result {
                        Ok(_content) => {
                            // Agent sends Done internally on success.
                        }
                        Err(e) => {
                            let _ = tx.send(AgentEvent::Token(format!("\n✗ Error: {e}\n")));
                            let _ = tx.send(AgentEvent::Done);
                        }
                    }
                });

                current_run = Some(handle);
            }

            TuiCommand::CancelGeneration => {
                if let Some(h) = current_run.take() {
                    h.abort();
                }
                // Send cancellation notice to the TUI.
                let _ = agent_tx.send(AgentEvent::Token("\n[Cancelled]\n".to_string()));
                let _ = agent_tx.send(AgentEvent::Done);
            }

            TuiCommand::ClearConversation => {
                // Cancel any active generation.
                if let Some(h) = current_run.take() {
                    h.abort();
                }

                // Drain memory — preserve only System messages.
                let mut mem = memory.write().unwrap();
                let system_msgs: Vec<Message> = mem
                    .to_context_vec()
                    .into_iter()
                    .filter(|m| m.role == Role::System)
                    .collect();
                *mem = Memory::new();
                for msg in system_msgs {
                    mem.push(msg);
                }
                drop(mem); // release write lock before read-lock for save

                // Persist the cleared state.
                {
                    let mem = memory.read().unwrap();
                    let name = persistence::default_thread_name(&workspace_root);
                    let _ = persistence::save_conversation(&name, &workspace_root, &mem);
                }
            }

            TuiCommand::ShellConfirmation {
                tool_call_id,
                approved,
            } => {
                // Relay user's decision to the agent via the oneshot channel.
                if let Some(pc) = agent.pending_confirmations() {
                    let mut map = pc.lock().unwrap();
                    if let Some(sender) = map.remove(&tool_call_id) {
                        let _ = sender.send(approved);
                    }
                }
            }

            TuiCommand::RunShell(command) => {
                // Execute the shell command asynchronously — do NOT block
                // the agent handler or the TUI thread. The command runs
                // in a blocking thread; when it completes, output is
                // pushed to memory and sent to the TUI for display.
                //
                // Send an immediate "Running" event so the user knows
                // the command is in progress (not frozen).
                let _ = agent_tx.send(AgentEvent::ShellRunning {
                    command: command.clone(),
                });

                let tx = agent_tx.clone();
                let mem = memory.clone();
                let ws = workspace_root.clone();
                let cmd_for_blocking = command.clone();

                tokio::spawn(async move {
                    let output = tokio::task::spawn_blocking(move || {
                        execute_shell_command(&cmd_for_blocking, &ws)
                    })
                    .await
                    .unwrap_or_else(|e| format!("Task panicked: {e}"));

                    // Push into shared memory so the LLM sees it
                    {
                        let mut mem = mem.write().unwrap();
                        mem.push(Message::new(
                            Role::User,
                            format!(
                                "User ran shell command: `{}`\n\nOutput:\n{}",
                                command, output
                            ),
                        ));
                    }

                    // Send to TUI for display
                    let _ = tx.send(AgentEvent::ShellOutput { command, output });
                });
            }

            TuiCommand::Exit => {
                // Save conversation before exiting.
                {
                    let mem = memory.read().unwrap();
                    let name = persistence::default_thread_name(&workspace_root);
                    let _ = persistence::save_conversation(&name, &workspace_root, &mem);
                }

                if let Some(h) = current_run.take() {
                    h.abort();
                }
                break;
            }
        }
    }
}

// ── Shell Execution Helper ────────────────────────────────────────────────────────

/// Executes a shell command in the workspace root, capturing stdout and stderr.
///
/// On Windows, uses `cmd /C` for near-instant startup (unlike PowerShell which
/// loads .NET CLR on every invocation). Encoding is handled via
/// [`decode_windows_stdout`], which tries UTF-8 first and falls back to the
/// system ANSI code page.
fn execute_shell_command(command: &str, workspace_root: &Path) -> String {
    #[cfg(target_os = "windows")]
    let (shell, shell_arg) = ("cmd", "/C");
    #[cfg(not(target_os = "windows"))]
    let (shell, shell_arg) = ("sh", "-c");

    let child = match Command::new(shell)
        .arg(shell_arg)
        .arg(command)
        .current_dir(workspace_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("Failed to spawn command: {e}"),
    };

    let pid = child.id();

    // Watchdog: polls every 100ms, kills the process if it exceeds the
    // timeout. An AtomicBool signal lets it exit early when the command
    // completes quickly — without this, join() would block for the full
    // timeout duration even for a 15ms `dir`.
    let done = Arc::new(AtomicBool::new(false));
    let done_signal = Arc::clone(&done);

    let timeout = Duration::from_secs(30);
    let watchdog = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if done_signal.load(Ordering::Relaxed) {
                return; // command finished, no kill needed
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Timeout reached — best-effort kill.
        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    });

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return format!("Failed to wait on command: {e}"),
    };

    // Signal the watchdog that the command is done, then join.
    // The watchdog checks the flag every 100ms, so join returns
    // within 100ms instead of blocking for the full 30s timeout.
    done.store(true, Ordering::Relaxed);
    let _ = watchdog.join();

    let stdout = decode_stdout(&output.stdout);
    let stderr = decode_stdout(&output.stderr);
    let exit_code = output.status.code();

    let stdout_clean = stdout.trim_end();
    let stderr_clean = stderr.trim_end();

    let mut result = String::new();
    if !stdout_clean.is_empty() {
        result.push_str(stdout_clean);
    }
    if !stderr_clean.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(stderr_clean);
    }

    // If nothing was produced, indicate the command ran
    if result.is_empty() {
        match exit_code {
            Some(0) => result.push_str("(command completed with no output)"),
            Some(code) => {
                result.push_str(&format!("(exit code: {code}, no output)"));
            }
            None => result.push_str("(process terminated by signal, no output)"),
        }
    } else if let Some(code) = exit_code
        && code != 0
    {
        result.push_str(&format!("\n\n[exit code: {code}]"));
    }

    result
}

/// Decodes child-process stdout/stderr bytes to a Rust string.
///
/// On Windows, many CLI tools (especially cmd built-ins like `dir`, `echo`,
/// and older programs) output in the system ANSI code page (e.g. GBK/CP936 for
/// Chinese-locale machines). Modern tools (git, cargo, rustc, python 3.7+)
/// typically output UTF-8 when stdout is not a TTY.
///
/// Strategy: try UTF-8 first. If every byte is valid UTF-8, use it. Otherwise
/// use the Windows [`GetACP`] code page via [`MultiByteToWideChar`]. On Unix
/// this is just [`String::from_utf8_lossy`].
#[cfg(target_os = "windows")]
fn decode_stdout(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Try UTF-8 first — modern tools output valid UTF-8.
    if let Ok(utf8) = std::str::from_utf8(bytes) {
        return utf8.to_string();
    }
    // Fall back to the system ANSI code page (e.g. CP936 for zh-CN).
    unsafe {
        let acp = GetACP();
        // CP 65001 IS UTF-8 — if the system already uses UTF-8, just
        // replace invalid sequences (shouldn't happen if from_utf8 failed).
        if acp == 65001 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        // Determine how many UTF-16 code units we need.
        let wide_len = MultiByteToWideChar(
            acp,
            0,
            bytes.as_ptr() as *const i8,
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if wide_len <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide: Vec<u16> = vec![0; wide_len as usize];
        let written = MultiByteToWideChar(
            acp,
            0,
            bytes.as_ptr() as *const i8,
            bytes.len() as i32,
            wide.as_mut_ptr(),
            wide_len,
        );
        if written <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        wide.truncate(written as usize);
        String::from_utf16_lossy(&wide)
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn GetACP() -> u32;
    fn MultiByteToWideChar(
        CodePage: u32,
        dwFlags: u32,
        lpMultiByteStr: *const i8,
        cbMultiByte: i32,
        lpWideCharStr: *mut u16,
        cchWideChar: i32,
    ) -> i32;
}

#[cfg(not(target_os = "windows"))]
fn decode_stdout(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
