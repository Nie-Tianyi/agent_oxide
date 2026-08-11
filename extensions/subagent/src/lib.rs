#![deny(unsafe_code)]
//! # Subagent — Spawn child agents as tools
//!
//! Provides [`SubagentTool`] — a [`Tool`](tools::Tool) that spawns a fresh
//! [`Agent`](engine::Agent) to complete complex sub-tasks.  When the parent
//! LLM calls the `task` tool, a child agent is created with its own memory
//! and a filtered tool set, runs to completion, and streams progress back.
//!
//! # Quick start
//!
//! ```ignore
//! use subagent::{SubagentTool, SubagentConfig, filter_tools};
//!
//! // Build a read-only tool set for sub-agents.
//! let subagent_tools = Arc::new(filter_tools(&parent_registry, &["read", "grep", "glob"]));
//!
//! let tool = SubagentTool::new(
//!     llm_client,
//!     SubagentConfig {
//!         model: "flash-model".into(),
//!         ..Default::default()
//!     },
//!     subagent_tools,
//!     parent_memory,
//! );
//! ```

mod config;
mod filter;
mod tool;

pub use config::SubagentConfig;
pub use filter::filter_tools;
pub use tool::SubagentTool;
