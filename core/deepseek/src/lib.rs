#![deny(unsafe_code)]
//! # DeepSeek — API client
//!
//! Concrete implementation of [`provider::LLMClient`] for the DeepSeek API.
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
