//! Error types for the tools system.

use thiserror::Error;

/// Error produced during tool execution.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ToolError {
    /// Tool runtime error (division by zero, invalid expression, etc.).
    #[error("tool execution error: {0}")]
    Execution(String),
    /// Invalid arguments — JSON parse failure, missing required field, wrong type.
    #[error("invalid tool arguments: {0}")]
    InvalidArgs(String),
}
