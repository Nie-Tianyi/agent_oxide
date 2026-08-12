//! # Agent Oxide
//!
//! A modular agent framework for Rust — ReAct loop engine, LLM provider
//! abstraction, tool system, and a rich set of extensions (sandbox,
//! persistence, skills, observability, subagents).
//!
//! Everything lives in one crate; the [`proc_macro`] companion
//! `agent-oxide-macros` supplies `#[derive(Agent)]`, `#[agent_impl]`, and
//! `#[tool]`:
//!
//! ```toml
//! [dependencies]
//! agent_oxide = "0.5"
//! agent-oxide-macros = "0.5"
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use agent_oxide::prelude::*;
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let client = DeepSeekClient::new(std::env::var("DEEPSEEK_API")?);
//!
//! let agent = Agent::builder(client, "deepseek-chat")
//!     .system_prompt("You are a helpful assistant.")
//!     .build();
//!
//! let answer = agent.run("What is 2+2?").await?;
//! println!("{answer}");
//! # Ok(())
//! # }
//! ```
//!
//! # Module layout
//!
//! | Module | Role |
//! |--------|------|
//! | [`provider`] | `LLMClient` trait and shared types (`Message`, `ToolCall`, …) |
//! | [`deepseek`] | DeepSeek API client |
//! | [`memory`] | Conversation memory buffer |
//! | [`tools`] | `Tool` trait, `ToolRegistry`, `#[tool]` macro |
//! | [`engine`] | Agent ReAct loop, `AgentHook`, `AgentEvent` |
//! | [`util`] | Shared utilities (`iso8601_now`) |
//! | [`agent_kit`] | NVIDIA OO Agents-style ergonomics |
//! | [`hooks`] | Compaction hooks (micro/macro) |
//! | [`observability`] | Full-chain tracing |
//! | [`persistence`] | Conversation save/load |
//! | [`sandbox`] | 5-layer security sandbox |
//! | [`skills`] | Skill discovery & registry |
//! | [`subagent`] | Spawn child agents as tools |

#![deny(unsafe_code)]

// ── Core modules ──────────────────────────────────────────────────────────────

/// LLM provider abstraction — `LLMClient` trait and shared types.
pub mod provider;
pub use provider::{
    Choice, ChoiceMessage, ChunkChoice, CompletionRequest, CompletionResponse, Delta, FinishReason,
    FunctionDef, LLMClient, Message, ProviderError, ReasoningEffort, Role, StreamChunk, ToolCall,
    ToolCallFunction, ToolCallKind, ToolChoice, ToolChoiceFunction, ToolDef, ToolDefKind, Usage,
};

/// DeepSeek API client — implements `provider::LLMClient`.
pub mod deepseek;
pub use deepseek::{DeepSeekClient, DeepSeekError, DeepSeekRequest, DeepSeekStream};

/// Conversation memory management.
pub mod memory;
pub use memory::{Memory, PendingHints, SharedMemory};

/// Tool abstraction — `Tool` trait, `ToolRegistry`, and JSON Schema generation.
pub mod tools;
pub use tools::{Progress, ProgressStream, Tool, ToolError, ToolRegistry, tool_to_def};

/// Agent engine — ReAct loop, `AgentHook` lifecycle, `AgentEvent` streaming.
pub mod engine;
pub use engine::{
    Agent, AgentBuilder, AgentError, AgentEvent, AgentHook, CallOrigin, EngineContext,
    EngineContextBuilder, InterventionRequest, InterventionResponse, ResponseRouter, RunOutcome,
    block_on, next_request_id,
};

/// Shared workspace utilities.
pub mod util;
pub use util::iso8601_now;

// ── Extension modules ─────────────────────────────────────────────────────────

/// NVIDIA OO Agents-style ergonomics on top of the core API.
pub mod agent_kit;
pub use agent_kit::{AgentAssembler, AgentBlueprint, BuildConfig, ContextBlock, ContextBlockHook};

/// Common hooks — compaction, approval, etc.
pub mod hooks;
/// Full-chain tracing — `TraceEvent`, `TraceStore`, `RunMetrics`.
pub mod observability;
/// Conversation persistence — save/load threads.
pub mod persistence;
/// 5-layer security sandbox — `WorkspaceFs`, `ShellFilter`, `SandboxHook`, …
pub mod sandbox;
/// Skill definitions, discovery, and registry.
pub mod skills;
/// Subagent as Tool — spawn autonomous sub-agents.
pub mod subagent;

// ── Proc macros and their support crates ──────────────────────────────────────

/// The `#[derive(Agent)]` / `#[agent_impl]` / `#[tool]` proc macros.
pub use agent_oxide_macros::{Agent, agent_impl, tool};

/// Re-exported for macro-generated code and ergonomic derives.
pub use schemars;
pub use serde;
pub use serde_json;

// ── Prelude ─────────────────────────────────────────────────────────────────────

/// Convenience prelude — the most commonly used types and macros.
pub mod prelude {
    pub use crate::{
        Agent, AgentBuilder, AgentHook, DeepSeekClient, LLMClient, Memory, Message, Role,
        SharedMemory, Tool, ToolRegistry, agent_impl, agent_kit, deepseek, engine, hooks, memory,
        observability, persistence, provider, sandbox, skills, subagent, tool, tools, util,
    };
}
