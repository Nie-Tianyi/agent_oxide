//! # Agent Oxide
//!
//! A modular agent framework for Rust — ReAct loop engine, LLM provider
//! abstraction, tool system, and a rich set of extensions (sandbox,
//! persistence, skills, observability, subagents).
//!
//! This umbrella crate re-exports the whole framework under one name:
//!
//! ```toml
//! [dependencies]
//! agent_oxide = "0.5"
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
//! # Crate layout
//!
//! | Module | Crate | Role |
//! |--------|-------|------|
//! | [`provider`] | `provider` | `LLMClient` trait and shared types (`Message`, `ToolCall`, …) |
//! | [`deepseek`] | `deepseek` | DeepSeek API client |
//! | [`memory`] | `memory` | Conversation memory buffer |
//! | [`tools`] | `tools` | `Tool` trait, `ToolRegistry`, `#[tool]` macro |
//! | [`engine`] | `engine` | Agent ReAct loop, `AgentHook`, `AgentEvent` |
//! | [`util`] | `util` | Shared utilities (`iso8601_now`) |
//! | [`agent_kit`] | `agent-kit` | NVIDIA OO Agents-style ergonomics |
//! | [`hooks`] | `hooks` | Compaction hooks (micro/macro) |
//! | [`observability`] | `observability` | Full-chain tracing |
//! | [`persistence`] | `persistence` | Conversation save/load |
//! | [`sandbox`] | `sandbox` | 5-layer security sandbox |
//! | [`skills`] | `skills` | Skill discovery & registry |
//! | [`subagent`] | `subagent` | Spawn child agents as tools |

#![deny(unsafe_code)]

// ── Core ────────────────────────────────────────────────────────────────────────

/// LLM provider abstraction — `LLMClient` trait and shared types.
pub use provider;
pub use provider::{
    Choice, ChoiceMessage, ChunkChoice, CompletionRequest, CompletionResponse, Delta,
    FinishReason, FunctionDef, LLMClient, Message, ProviderError, ReasoningEffort, Role,
    StreamChunk, ToolCall, ToolCallFunction, ToolCallKind, ToolChoice, ToolChoiceFunction,
    ToolDef, ToolDefKind, Usage,
};

/// DeepSeek API client — implements `provider::LLMClient`.
pub use deepseek;
pub use deepseek::{DeepSeekClient, DeepSeekError, DeepSeekRequest, DeepSeekStream};

/// Conversation memory management.
pub use memory;
pub use memory::{Memory, PendingHints, SharedMemory};

/// Tool abstraction — `Tool` trait, `ToolRegistry`, and JSON Schema generation.
pub use tools;
pub use tools::{Progress, ProgressStream, Tool, ToolError, ToolRegistry, tool_to_def};

/// The `#[tool]` attribute macro.
pub use tools_macros::tool;

/// Agent engine — ReAct loop, `AgentHook` lifecycle, `AgentEvent` streaming.
pub use engine;
pub use engine::{
    Agent, AgentBuilder, AgentError, AgentEvent, AgentHook, CallOrigin, EngineContext,
    EngineContextBuilder, InterventionRequest, InterventionResponse, ResponseRouter,
    RunOutcome, block_on, next_request_id,
};

/// Shared workspace utilities.
pub use util;
pub use util::iso8601_now;

// ── Extensions ──────────────────────────────────────────────────────────────────

/// NVIDIA OO Agents-style ergonomics on top of the core API.
pub use agent_kit;
pub use agent_kit::{AgentAssembler, AgentBlueprint, BuildConfig, ContextBlock, ContextBlockHook};
/// The `#[derive(Agent)]` / `#[agent_impl]` proc macros.
pub use agent_macros::Agent;

/// Common hooks — compaction, approval, etc.
pub use hooks;
/// Full-chain tracing — `TraceEvent`, `TraceStore`, `RunMetrics`.
pub use observability;
/// Conversation persistence — save/load threads.
pub use persistence;
/// 5-layer security sandbox — `WorkspaceFs`, `ShellFilter`, `SandboxHook`, …
pub use sandbox;
/// Skill definitions, discovery, and registry.
pub use skills;
/// Subagent as Tool — spawn autonomous sub-agents.
pub use subagent;

// ── Prelude ─────────────────────────────────────────────────────────────────────

/// Convenience prelude — the most commonly used types and macros.
pub mod prelude {
    pub use crate::{
        Agent, AgentBuilder, AgentHook, DeepSeekClient, LLMClient, Memory, Message, Role,
        SharedMemory, Tool, ToolRegistry, agent_kit, deepseek, engine, hooks, memory,
        observability, persistence, provider, sandbox, skills, subagent, tool, tools, util,
    };
}
