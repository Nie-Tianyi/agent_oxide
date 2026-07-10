//! # Tools — abstraction layer
//!
//! Defines the [`Tool`] trait, [`ToolRegistry`] container, [`WorkspaceFs`]
//! sandbox, and JSON Schema generation helpers.
//!
//! Concrete tool implementations live in downstream crates (e.g. `loomis`).

mod error;
mod fs;
mod progress;
mod registry;
pub mod sandbox;
mod schema;
mod tool;

pub use error::{FsError, ToolError};
pub use fs::{DirEntry, EntryType, GrepMatch, WorkspaceFs};
pub use progress::{Progress, ProgressStream};
pub use registry::{ToolRegistry, tool_to_def};
pub use sandbox::SandboxConfig;
pub use schema::generate_schema;
pub use tool::Tool;
pub use tools_macros::tool;
