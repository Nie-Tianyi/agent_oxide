#![deny(unsafe_code)]
//! # Provider — LLM abstraction layer
//!
//! This crate defines the [`LLMClient`] trait and all shared types needed
//! by the rest of the agent framework: messages, tool definitions, request
//! and response structures, and streaming primitives.
//!
//! Concrete provider implementations (e.g. [`deepseek`]) live in sibling
//! crates and implement [`LLMClient`].

mod client;
mod error;
mod message;
mod request;
mod response;
mod stream;
mod tool_def;

pub use client::LLMClient;
pub use error::ProviderError;
pub use message::{Message, Role, ToolCall, ToolCallFunction, ToolCallKind};
pub use request::{CompletionRequest, ReasoningEffort};
pub use response::{Choice, ChoiceMessage, CompletionResponse, FinishReason, Usage};
pub use stream::{ChunkChoice, Delta, StreamChunk};
pub use tool_def::{FunctionDef, ToolChoice, ToolChoiceFunction, ToolDef, ToolDefKind};
