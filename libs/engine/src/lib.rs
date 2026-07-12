#![deny(unsafe_code)]
//! # Engine — Agent ReAct Loop
//!
//! Core agent loop with [`AgentHook`] lifecycle and streaming [`AgentEvent`]s.
//!
//! # Quick start
//!
//! ```ignore
//! use engine::Agent;
//!
//! let agent = Agent::builder(client, "deepseek-v4")
//!     .system_prompt("You are a helpful assistant.")
//!     .tool(calculator_tool)
//!     .build();
//!
//! let answer = agent.run("What is 2+2?").await?;
//! ```
//!
//! # Key types
//!
//! | Type | Role |
//! |------|------|
//! | [`Agent`] | The ReAct loop — call [`run`](Agent::run) or [`run_with_events`](Agent::run_with_events) |
//! | [`AgentBuilder`] | Fluent constructor — the primary entry point for most users |
//! | [`EngineContext`] | Configuration bag — use [`EngineContext::builder`] for the advanced API |
//! | [`EngineContextBuilder`] | Builder for `EngineContext` — advanced users only |
//! | [`AgentHook`] | Lifecycle callbacks — implement to observe or intercept agent actions |
//! | [`AgentEvent`] | Streaming events — Token, ToolCall, ToolSuccessful, ToolFailure, Done, ... |
//! | [`AgentError`] | Error type — Provider, Tool, MaxStepsReached, ... |

mod agent;
mod builder;
mod context;
mod hooks;
mod response_router;

pub use agent::{
    Agent, AgentError, AgentEvent, CallOrigin, InterventionRequest, InterventionResponse,
    RunOutcome,
};
pub use builder::AgentBuilder;
pub use context::{EngineContext, EngineContextBuilder};
pub use hooks::AgentHook;
pub use response_router::{ResponseRouter, next_request_id};
