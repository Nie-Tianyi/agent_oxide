//! Observability crate for the Loomis agent framework.
//!
//! Provides:
//!
//! - [`TraceEvent`] — granular lifecycle events (LLM calls, tool executions, …).
//! - [`TraceStore`] — dispatches events to the [`tracing`] infrastructure.
//! - [`RunMetrics`] — aggregated counters and timing data for the TUI status bar.
//! - [`ObservabilityHook`] — [`AgentHook`](engine::AgentHook) that populates a [`TraceStore`].

pub mod event;
pub mod hook;
pub mod store;

pub use event::TraceEvent;
pub use hook::ObservabilityHook;
pub use store::{RunMetrics, TraceStore};
