#![deny(unsafe_code)]
//! # Agent Oxide Macros
//!
//! The proc macros behind the Agent Oxide framework:
//!
//! - [`#[tool]`](attr@tool) — generate a `Tool` trait implementation from a
//!   struct plus `name` / `description` / `args` parameters, with lazily
//!   derived JSON Schema via `schemars`.
//! - [`#[derive(Agent)]`](derive@Agent) — NVIDIA OO Agents-style ergonomics:
//!   annotate a struct whose doc comment becomes the system prompt;
//!   `#[tool]` fields are auto-registered; `#[agent(client)]` fields supply
//!   the LLM client; a generated `into_agent(model)` assembles a
//!   `agent_oxide::engine::Agent`.
//! - [`#[agent_impl]`](attr@agent_impl) — annotate an `impl` block.
//!   **Synchronous** methods become LLM-callable tools (their doc comment is
//!   the tool description). **Async** methods with an empty body `{}` become
//!   *generation methods*: the macro replaces the body with an LLM call,
//!   mirroring Python's `...` sentinel.
//!
//! All generated code references paths through `agent_oxide` (e.g.
//! `agent_oxide::agent_kit::serde`), so users only need `agent_oxide` (and
//! `agent-oxide-macros` itself) as a dependency.
//!
//! ```ignore
//! use agent_oxide_macros::{Agent, agent_impl, tool};
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
//! ```

use proc_macro::TokenStream;

mod agent_derive;
mod agent_impl;
mod tool;
mod util;

/// Derive macro for agent structs (see crate docs).
#[proc_macro_derive(Agent, attributes(agent, tool, context, strategy))]
pub fn derive_agent(input: TokenStream) -> TokenStream {
    agent_derive::expand(input)
}

/// Attribute macro for agent `impl` blocks (see crate docs).
#[proc_macro_attribute]
pub fn agent_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    agent_impl::expand(attr, item)
}

/// Attribute macro that generates a `Tool` trait implementation (see crate docs).
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    tool::expand(attr, item)
}
