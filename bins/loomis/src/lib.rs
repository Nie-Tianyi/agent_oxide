//! Loomis — concrete agent implementation with DeepSeek, tools, hooks, and TUI.

pub mod app;
pub mod hooks;
pub mod sandbox;
pub mod tools;
pub mod tui;

pub use app::{AgentEvent, AgentKit, InterveneResponse, build_coding_agent};
