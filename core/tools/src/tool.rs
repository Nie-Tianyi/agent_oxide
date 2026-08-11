//! [`Tool`] trait — the core abstraction for LLM-callable tools.
//!
//! ## Why sync?
//!
//! The trait methods are synchronous (`fn execute`, not `async fn execute`).
//! This is intentional:
//!
//! - **Object-safe** — sync traits naturally support `dyn Tool` without `async_trait`.
//! - **All current tools are CPU-bound** (expression eval, string ops, file I/O).
//! - **Extensible** — if async tools are needed later, wrap in `spawn_blocking`
//!   or add an `AsyncTool` sub-trait.

use serde_json::Value;

use super::ToolError;
use super::progress::ProgressStream;

/// A tool that can be called by an LLM.
///
/// # Required methods
///
/// | Method | Purpose |
/// |--------|---------|
/// | [`name`](Tool::name) | Tool name, maps to `function.name` in API requests |
/// | [`description`](Tool::description) | Human-readable description for the model |
/// | [`parameter_schema`](Tool::parameter_schema) | JSON Schema for the tool's arguments |
/// | [`execute_stream`](Tool::execute_stream) | Execute and return a [`ProgressStream`] |
///
/// # Progress streaming
///
/// Every tool returns a [`ProgressStream`] — a boxed, `Send`-able
/// [`Stream`](futures_core::Stream) of [`Progress`] events.  Short-lived
/// tools (calculator, file ops) emit a single [`Progress::Done`] event.
/// Long-running tools (shell) emit [`Progress::InProgress`] updates
/// followed by [`Progress::Done`].
pub trait Tool: Send + Sync {
    /// Tool name — used as `function.name` in API requests.
    fn name(&self) -> &str;

    /// Human-readable description shown to the model.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's expected arguments.
    fn parameter_schema(&self) -> Value;

    /// Execute the tool and return a stream of progress events.
    ///
    /// The final event in the stream **must** be [`Progress::Done`].
    fn execute_stream(&self, args: &str) -> Result<ProgressStream, ToolError>;

    /// Convert to a [`provider::ToolDef`] for API requests.
    fn to_def(&self) -> provider::ToolDef {
        provider::ToolDef {
            kind: provider::ToolDefKind::Function,
            function: provider::FunctionDef {
                name: self.name().to_owned(),
                description: Some(self.description().to_owned()),
                parameters: Some(self.parameter_schema()),
            },
        }
    }
}
