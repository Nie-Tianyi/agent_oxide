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
//! agent_rx ←────── AgentEvent ─────── agent_tx (Agent loop + SandboxHook + user cmds)
//! ```

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use deepseek::DeepSeekClient;
use engine::{Agent, AgentEvent, CallOrigin, InterveneResponse};
use memory::SharedMemory;
use provider::{Message, Role};

use super::app::App;
use super::messages::TuiCommand;
use super::shell_exec::execute_shell_command;
use crate::app::AgentKit;

// ── Entry Point ──────────────────────────────────────────────────────────────────

/// Initialises the TUI and runs the event loop until the user exits.
///
/// This function is **synchronous** — it blocks the calling thread until
/// the user types `/exit` or presses Ctrl+C/D. The caller must already be
/// inside a tokio runtime (e.g. via `#[tokio::main]`).
pub fn run(kit: AgentKit, workspace_root: PathBuf, model: &str) -> io::Result<()> {
    // ── Destructure the kit ─────────────────────────────────────
    let AgentKit {
        agent,
        memory,
        tool_names,
        model: _kit_model,
        agent_rx,
        agent_tx,
        intervene_tx,
    } = kit;

    // ── Create command channel ────────────────────────────────────
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();

    // ── Spawn agent handler ─────────────────────────────────────────
    tokio::spawn(agent_handler(
        Arc::new(agent),
        memory.clone(),
        cmd_rx,
        agent_tx,
        workspace_root.clone(),
        intervene_tx,
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
    let mut agent_rx = agent_rx;
    let mut pending_events: Vec<AgentEvent> = Vec::new();

    loop {
        // ── Render ───────────────────────────────────────────────────
        terminal.draw(|frame| super::ui::draw(frame, app))?;

        // ── Poll keyboard ────────────────────────────────────────────
        if crossterm::event::poll(std::time::Duration::from_millis(50))? {
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

        // ── Drain agent events (single channel for everything) ─────
        while let Ok(event) = agent_rx.try_recv() {
            pending_events.push(event);
        }

        // Apply all agent events together
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
    agent: Arc<Agent<DeepSeekClient>>,
    memory: SharedMemory,
    mut cmd_rx: UnboundedReceiver<TuiCommand>,
    agent_tx: UnboundedSender<AgentEvent>,
    workspace_root: PathBuf,
    intervene_tx: std::sync::mpsc::SyncSender<InterveneResponse>,
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
                    let result = agent.run_with_events(&input, tx.clone()).await;

                    // Auto-save conversation after each agent turn.
                    {
                        let mem = mem_for_save.read().unwrap();
                        let name = memory::default_thread_name(&ws);
                        let _ = memory::save_conversation(&name, &ws, &mem);
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
                *mem = memory::Memory::new();
                for msg in system_msgs {
                    mem.push(msg);
                }
                drop(mem); // release write lock before read-lock for save

                // Persist the cleared state.
                {
                    let mem = memory.read().unwrap();
                    let name = memory::default_thread_name(&workspace_root);
                    let _ = memory::save_conversation(&name, &workspace_root, &mem);
                }
            }

            TuiCommand::InterveneResponse {
                request_id: _,
                response,
            } => {
                // Unblock the synchronous intervention hook waiting in
                // SandboxHook::before_tool_call.
                let _ = intervene_tx.send(response);
            }

            TuiCommand::RunShell(command) => {
                // Execute the shell command asynchronously — do NOT block
                // the agent handler or the TUI thread. The command runs
                // in a blocking thread; when it completes, output is
                // pushed to memory and sent to the TUI for display.
                //
                // Use unified ToolCall / ToolResult events with User origin
                // instead of the old ShellRunning / ShellOutput events.
                let shell_id = format!(
                    "shell-{:x}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );

                // Notify TUI that the command is starting.
                let _ = agent_tx.send(AgentEvent::ToolCall {
                    id: shell_id.clone(),
                    name: "shell".into(),
                    arguments: command.clone(),
                    origin: CallOrigin::User,
                });

                let tx = agent_tx.clone();
                let mem = memory.clone();
                let ws = workspace_root.clone();
                let cmd_for_blocking = command.clone();
                let sid = shell_id.clone();

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

                    // Send result to TUI for display
                    let _ = tx.send(AgentEvent::ToolResult {
                        id: sid,
                        name: "shell".into(),
                        output,
                    });
                });
            }

            TuiCommand::Exit => {
                // Save conversation before exiting.
                {
                    let mem = memory.read().unwrap();
                    let name = memory::default_thread_name(&workspace_root);
                    let _ = memory::save_conversation(&name, &workspace_root, &mem);
                }

                if let Some(h) = current_run.take() {
                    h.abort();
                }
                break;
            }
        }
    }
}
