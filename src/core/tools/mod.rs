//! # Tools — abstraction layer
//!
//! Defines the [`Tool`] trait, [`ToolRegistry`] container, and JSON Schema
//! generation helpers.
//!
//! Concrete tool implementations live in downstream crates (e.g. your binary).

mod error;
mod progress;
mod registry;
mod schema;
mod tool;

pub use agent_oxide_macros::tool;
pub use error::ToolError;
pub use progress::{Progress, ProgressStream};
pub use registry::{ToolRegistry, tool_to_def};
pub use schema::generate_schema;
pub use tool::Tool;
