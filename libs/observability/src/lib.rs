//! Observability crate for the Loomis agent framework.
//!
//! Provides full-chain tracing of agent internal state:
//!
//! - [`TraceEvent`] — granular lifecycle events (LLM calls, tool executions, …).
//! - [`TraceStore`] — thread-safe event collector with a lock-free ring buffer.
//! - [`RunMetrics`] — aggregated counters and timing data.
//! - [`SubagentTrace`] — summary of child agent execution.
//!
//! The crate is designed to be shared between the agent task (writes)
//! and the TUI render loop (reads) via `Arc<TraceStore>`.

pub mod event;
pub mod store;
pub mod subagent;

pub use event::{Timestamped, TraceEvent};
pub use store::{RunMetrics, TraceStore};
pub use subagent::SubagentTrace;
