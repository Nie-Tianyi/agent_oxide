//! Concrete [`AgentHook`](engine::AgentHook) implementations.

mod response_router;
mod sandbox_hook;

pub use response_router::{ResponseRouter, next_request_id};
pub use sandbox_hook::SandboxHook;
