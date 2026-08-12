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
//! The source is organized as [`core`] (engine layer) and [`extensions`]
//! (optional capabilities); every module is re-exported at the crate root,
//! so the public API is flat — `agent_oxide::provider`, `agent_oxide::sandbox`, …
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

// ── Core modules (engine layer, src/core/) ─────────────────────────────────────

pub mod core;

/// LLM provider abstraction — `LLMClient` trait and shared types.
pub use core::provider;
pub use provider::{
    Choice, ChoiceMessage, ChunkChoice, CompletionRequest, CompletionResponse, Delta, FinishReason,
    FunctionDef, LLMClient, Message, ProviderError, ReasoningEffort, Role, StreamChunk, ToolCall,
    ToolCallFunction, ToolCallKind, ToolChoice, ToolChoiceFunction, ToolDef, ToolDefKind, Usage,
};

/// DeepSeek API client — implements `provider::LLMClient`.
pub use core::deepseek;
pub use deepseek::{DeepSeekClient, DeepSeekError, DeepSeekRequest, DeepSeekStream};

/// Conversation memory management.
pub use core::memory;
pub use memory::{Memory, PendingHints, SharedMemory};

/// Tool abstraction — `Tool` trait, `ToolRegistry`, and JSON Schema generation.
pub use core::tools;
pub use tools::{Progress, ProgressStream, Tool, ToolError, ToolRegistry, tool_to_def};

/// Agent engine — ReAct loop, `AgentHook` lifecycle, `AgentEvent` streaming.
pub use core::engine;
pub use engine::{
    Agent, AgentBuilder, AgentError, AgentEvent, AgentHook, CallOrigin, EngineContext,
    EngineContextBuilder, InterventionRequest, InterventionResponse, ResponseRouter, RunOutcome,
    block_on, next_request_id,
};

/// Shared workspace utilities.
pub use core::util;
pub use util::iso8601_now;

// ── Extension modules (src/extensions/) ────────────────────────────────────────

pub mod extensions;

pub use agent_kit::{AgentAssembler, AgentBlueprint, BuildConfig, ContextBlock, ContextBlockHook};
/// NVIDIA OO Agents-style ergonomics on top of the core API.
pub use extensions::agent_kit;

/// Common hooks — compaction, approval, etc.
pub use extensions::hooks;
/// Full-chain tracing — `TraceEvent`, `TraceStore`, `RunMetrics`.
pub use extensions::observability;
/// Conversation persistence — save/load threads.
pub use extensions::persistence;
/// 5-layer security sandbox — `WorkspaceFs`, `ShellFilter`, `SandboxHook`, …
pub use extensions::sandbox;
/// Skill definitions, discovery, and registry.
pub use extensions::skills;
/// Subagent as Tool — spawn autonomous sub-agents.
pub use extensions::subagent;

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
