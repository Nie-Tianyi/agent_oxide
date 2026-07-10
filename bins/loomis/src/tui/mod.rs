//! # TUI — Terminal User Interface
//!
//! A ratatui-based chat interface for Agent Oxide, inspired by Claude Code's
//! terminal UX: scrollable conversation history, real-time streaming tokens,
//! styled tool calls, and an interactive input area.
//!
//! ## Architecture
//!
//! ```text
//! main.rs ──→ tui::run() ──→ event_loop()
//!               │                │
//!               │                ├─ poll crossterm keys (50ms timeout)
//!               │                ├─ drain agent events via try_recv
//!               │                └─ render frame via ratatui
//!               │
//!               └── tokio::spawn(agent_handler)
//!                      │
//!                      └── loop { recv(cmd_rx) → run agent / cancel / clear }
//! ```
//!
//! ## Modules
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`app`] | `App` state machine + event application |
//! | [`messages`] | `ChatMessage`, `TuiCommand`, `ThreadPicker` type definitions |
//! | [`input`] | Keyboard handling, slash commands, shell confirmation |
//! | [`ui`] | ratatui rendering: chat area, input area, status bar |
//! | [`event`] | Event loop, terminal lifecycle, agent background task |
//! | [`shell_exec`] | User `!command` shell execution + Windows encoding |

mod app;
mod event;
mod input;
mod markdown;
mod messages;
mod shell_exec;
mod ui;

pub use app::App;
pub use event::run;
pub use messages::{ChatMessage, ThreadPicker, ToolCallState, TuiCommand};
