//! # Agent Kit — NVIDIA OO Agents-style ergonomics on top of the Agent Oxide core
//!
//! This crate maps the NVIDIA OO Agents programming paradigm onto the
//! existing Agent Oxide `core/` API **without modifying core**.
//!
//! The [`agent-macros`] proc-macro crate generates the code; this crate
//! provides the runtime support:
//!
//! | Module | Role |
//! |--------|------|
//! | [`blueprint`] | [`AgentBlueprint`] trait — how a user-defined struct describes itself |
//! | [`generation`] | [`run_generation`] — call the LLM for a "generation method" (empty async fn body) |
//! | [`builder`] | [`BuildConfig`] + [`AgentAssembler`] — assemble a `core::engine::Agent` |
//! | [`context`] | [`ContextBlock`] / [`ContextBlockHook`] — dynamic context injection |
//!
//! # Example
//!
//! ```ignore
//! use agent_oxide_macros::{Agent, agent_impl};
//!
//! /// You are an agent specializing in analyzing customer feedback.
//! #[derive(Agent)]
//! struct FeedbackAgent {
//!     #[agent(client)]
//!     client: DeepSeekClient,
//! }
//!
//! #[agent_impl]
//! impl FeedbackAgent {
//!     /// Analyze customer feedback for sentiment and key topics in one sentence.
//!     async fn analyze_feedback(&self, text: String) -> String {}
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let agent = FeedbackAgent {
//!         client: DeepSeekClient::new(api_key),
//!     }.into_agent("deepseek-v4-pro")?;
//!     let result = agent.analyze_feedback("Great product, but shipping was slow").await?;
//!     println!("{result}");
//!     Ok(())
//! }
//! ```

pub mod blueprint;
pub mod builder;
pub mod context;
pub mod generation;

pub use blueprint::AgentBlueprint;
pub use builder::{AgentAssembler, BuildConfig};
pub use context::{ContextBlock, ContextBlockHook};
pub use generation::{GenerationError, Strategy, run_generation};

// ── Re-exports ────────────────────────────────────────────────────────────────
//
// The `agent-macros` generated code references everything through
// `agent_kit::...` paths, so consuming crates need no direct dependencies
// on core crates, serde, or schemars. The `serde`/`schemars` re-exports
// also let users write `#[derive(agent_kit::serde::Deserialize, ...)]`
// (pair with `#[serde(crate = "agent_kit::serde")]` /
// `#[schemars(crate = "agent_kit::schemars")]` when deriving in a crate
// that doesn't depend on those crates directly).

pub use crate::engine;
pub use crate::memory;
pub use crate::provider;
pub use crate::tools;
pub use schemars;
pub use serde;
pub use serde_json;

/// Default model used by generation methods when no `#[agent(model)]`
/// field is present.
///
/// Re-exported from [`crate::deepseek::DEFAULT_MODEL`] — vendor-specific
/// defaults live in the vendor module, not in the generic agent_kit
/// layer.  Downstream applications should override it via
/// `#[agent(model = "...")]` or their own configuration.
pub use crate::deepseek::DEFAULT_MODEL;
