//! # Agent Macros — NVIDIA OO Agents-style ergonomics for Agent Oxide
//!
//! Two macros that translate the NVIDIA OO Agents writing style into
//! calls against the existing Agent Oxide `core/` API — **without modifying
//! core**:
//!
//! - [`#[derive(Agent)]`](derive@Agent) — annotate a struct. The struct's
//!   doc comment becomes the system prompt; `#[tool]` fields are
//!   auto-registered; `#[agent(client)]` fields supply the LLM client;
//!   a generated `into_agent(model)` assembles a `core::engine::Agent`.
//! - [`#[agent_impl]`](attr@agent_impl) — annotate an `impl` block.
//!   **Synchronous** methods become LLM-callable tools (their doc
//!   comment is the tool description). **Async** methods with an empty
//!   body `{}` become *generation methods*: the macro replaces the body
//!   with an LLM call, mirroring Python's `...` sentinel.
//!
//! ```ignore
//! use agent_macros::{Agent, agent_impl};
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
