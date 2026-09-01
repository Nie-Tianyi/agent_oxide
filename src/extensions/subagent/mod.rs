//! # Subagent — Spawn child agents as tools
//!
//! Provides [`SubagentTool`] — a [`Tool`](crate::tools::Tool) that spawns a fresh
//! [`Agent`](crate::engine::Agent) to complete complex sub-tasks.  When the parent
//! LLM calls the tool, a child agent is created with its own memory and a
//! filtered tool set, runs to completion, and streams progress back.
//!
//! Subagents are defined as Markdown files with YAML frontmatter (Claude
//! Code `agents/*.md` style): drop definitions into an `agents/` directory,
//! and each one becomes its own tool named after the definition, with the
//! definition's `description` as the routing signal the parent LLM sees.
//!
//! # Quick start
//!
//! ```ignore
//! use agent_oxide::subagent::{SubagentRegistry, register_subagents};
//!
//! let registry = SubagentRegistry::discover(&[PathBuf::from("./agents")]);
//! let builder = register_subagents(
//!     Agent::builder(client.clone(), model),
//!     client,
//!     &registry,
//!     &parent_registry,  // parent tools WITHOUT any subagent tools — recursion guard
//!     parent_memory,
//!     model,             // fallback model for defs that don't set one
//! );
//! let agent = builder.build();
//! ```
//!
//! A single definition can also be wired directly:
//!
//! ```ignore
//! use agent_oxide::subagent::{SubagentTool, SubagentDef};
//!
//! let tool = SubagentTool::new(client, def, &parent_registry, parent_memory, model);
//! ```
//!
//! See `docs/subagent-migration-guide.md` for the definition format.

mod config;
mod def;
mod filter;
mod registry;
mod tool;

pub use def::{SubagentDef, SubagentError};
pub use registry::{SubagentRegistry, register_subagents};
pub use tool::SubagentTool;
