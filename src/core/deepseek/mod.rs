//! # DeepSeek — API client
//!
//! Concrete implementation of [`crate::provider::LLMClient`] for the DeepSeek API.
//! Includes SSE streaming support and DeepSeek-specific request/response types.

mod client;
mod error;
mod request;
mod response;
mod stream;

pub use client::DeepSeekClient;
pub use error::DeepSeekError;
pub use request::{DeepSeekRequest, ResponseFormat, ResponseFormatType, Thinking, ThinkingMode};
pub use stream::DeepSeekStream;

/// Default DeepSeek model used when no `#[agent(model)]` field is present.
///
/// Vendor-specific defaults belong in the vendor module — the generic
/// layers (agent_kit, engine) re-export rather than define them.
pub const DEFAULT_MODEL: &str = "deepseek-v4-pro";
