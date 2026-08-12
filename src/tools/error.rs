//! Error types for the tools system.

use std::fmt;

/// Error produced during tool execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolError {
    /// Tool runtime error (division by zero, invalid expression, etc.).
    Execution(String),
    /// Invalid arguments — JSON parse failure, missing required field, wrong type.
    InvalidArgs(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Execution(reason) => write!(f, "tool execution error: {reason}"),
            Self::InvalidArgs(reason) => write!(f, "invalid tool arguments: {reason}"),
        }
    }
}

impl std::error::Error for ToolError {}
